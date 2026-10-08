//! Cross-platform streaming audio inputs: a looping WAV file and a deterministic
//! simulated signal. Both feed the same bounded PCM queue contract as the
//! Windows WASAPI loopback in `live`.
//!
//! System-audio capture on Linux (PipeWire/PulseAudio) is intentionally not
//! implemented in this slice; `live` keeps that boundary explicit. These sources
//! exist so the headless CLI and its tests exercise the whole transport path
//! with reproducible input instead of a microphone or desktop mixer.
use crate::{clock::unix_ns, rtp::FRAMES};
use anyhow::{Context, Result, ensure};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use serde::Serialize;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Longest accepted input file: ten minutes of stereo audio at the target rate.
const MAX_SECONDS: u64 = 600;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Loop a WAV file from disk, resampled to the streaming rate.
    File,
    /// Deterministic synthetic signal; no file access at all.
    Simulated,
}
impl Kind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "simulated" => Some(Self::Simulated),
            _ => None,
        }
    }
}

/// Same shape as `live::Metrics`; never report a capability that is not measured.
#[derive(Default, Clone, Serialize)]
pub struct Metrics {
    pub dropped_frames: u64,
    pub underrun_packets: u64,
    pub capture_frames: u64,
    pub max_queue_age_ms: f64,
    pub capture_to_send_p95_ms: Option<f64>,
    pub last_audio_qpc_ns: u64,
    pub input_rate: u32,
}

#[derive(Clone, Copy)]
struct Frame {
    left: f32,
    right: f32,
    stamp: u64,
}

struct State {
    equalizer: crate::equalizer::Processor,
    frames: VecDeque<Frame>,
    capacity: usize,
    target: usize,
    error: Option<String>,
    metrics: Metrics,
    ages: VecDeque<f64>,
}
impl State {
    fn push_frame(&mut self, frame: Frame) {
        if self.frames.len() >= self.capacity {
            self.frames.pop_front();
            self.metrics.dropped_frames += 1;
        }
        self.frames.push_back(frame);
    }
}

pub struct StreamSource {
    equalizer_control: crate::equalizer::Control,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl StreamSource {
    pub fn start(
        kind: Kind,
        path: &Path,
        rate: u32,
        equalizer: crate::equalizer::Control,
    ) -> Result<Self> {
        let path = path.to_path_buf();
        let capacity = rate as usize * 60 / 1000;
        let target = rate as usize * 20 / 1000;
        let state = Arc::new(Mutex::new(State {
            equalizer: crate::equalizer::Processor::new(equalizer.initial(), rate),
            frames: VecDeque::with_capacity(capacity),
            capacity,
            target,
            error: None,
            metrics: Metrics::default(),
            ages: VecDeque::with_capacity(4000),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let child_stop = stop.clone();
        let child_state = state.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("airflash-input".into())
            .spawn(move || {
                if let Err(e) = produce(kind, &path, rate, child_stop, &child_state, &tx) {
                    let message = format!("{e:#}");
                    child_state.lock().unwrap().error = Some(message.clone());
                    let _ = tx.try_send(Err(message));
                }
            })?;
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self {
                equalizer_control: equalizer,
                state,
                stop,
                worker: Some(worker),
            }),
            result => {
                stop.store(true, Ordering::Release);
                let _ = worker.join();
                anyhow::bail!("audio input initialization: {result:?}");
            }
        }
    }

    pub fn ready(&self) -> Result<bool> {
        let s = self.state.lock().unwrap();
        if let Some(e) = &s.error {
            anyhow::bail!("{e}");
        }
        Ok(s.frames.len() >= s.target)
    }

    pub fn packet(&self, gain: f32) -> Result<(Vec<u8>, Option<u64>)> {
        let mut s = self.state.lock().unwrap();
        if let Some((sequence, prepared)) = self.equalizer_control.latest() {
            s.equalizer.update(sequence, prepared);
        }
        if let Some(e) = &s.error {
            anyhow::bail!("audio input stopped: {e}");
        }
        let mut out = Vec::with_capacity(FRAMES * 4);
        let mut marker = None;
        let now = unix_ns();
        let mut underrun = false;
        for _ in 0..FRAMES {
            let frame = if let Some(f) = s.frames.pop_front() {
                f
            } else {
                underrun = true;
                Frame {
                    left: 0.0,
                    right: 0.0,
                    stamp: now,
                }
            };
            if frame.left.abs().max(frame.right.abs()) > 0.004 {
                s.metrics.last_audio_qpc_ns = frame.stamp;
                marker.get_or_insert(frame.stamp);
            }
            let age = now.saturating_sub(frame.stamp) as f64 / 1e6;
            s.metrics.max_queue_age_ms = s.metrics.max_queue_age_ms.max(age);
            if s.ages.len() == 4000 {
                s.ages.pop_front();
            }
            s.ages.push_back(age);
            for sample in s.equalizer.frame([frame.left, frame.right]) {
                let value = (sample * gain).clamp(-1.0, 1.0);
                out.extend(((value * 32767.0).round() as i16).to_be_bytes());
            }
        }
        if underrun {
            s.metrics.underrun_packets += 1;
        }
        Ok((out, marker))
    }

    /// Restore the capture queue to its normal target after a scheduler stall.
    pub fn discard_stale(&self) {
        let mut s = self.state.lock().unwrap();
        let cutoff = unix_ns().saturating_sub(20_000_000);
        while s.frames.front().is_some_and(|f| f.stamp < cutoff) || s.frames.len() > s.target {
            s.frames.pop_front();
            s.metrics.dropped_frames += 1;
        }
    }

    pub fn metrics(&self) -> Metrics {
        let s = self.state.lock().unwrap();
        let mut metrics = s.metrics.clone();
        let mut ages: Vec<_> = s.ages.iter().copied().collect();
        ages.sort_by(f64::total_cmp);
        metrics.capture_to_send_p95_ms = ages.get(ages.len() * 95 / 100).copied();
        metrics
    }
}
impl Drop for StreamSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

/// Decode a WAV file into deinterleaved stereo f32 plus its native sample rate.
/// Accepts 16/24/32-bit integer or 32-bit float PCM, mono or multi-channel;
/// the first two channels are used and mono is duplicated.
fn read_wav(path: &Path) -> Result<(Vec<Vec<f32>>, u32)> {
    let mut reader = hound::WavReader::open(path).context("open input WAV")?;
    let spec = reader.spec();
    let source_rate = spec.sample_rate;
    ensure!(
        (1..=192000).contains(&source_rate),
        "input WAV sample rate {} is out of range",
        source_rate
    );
    ensure!(spec.channels >= 1, "input WAV has no channels");
    // Normalize every supported layout to i32 range so deinterleaving has one scale.
    let frames: Vec<i32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|s| i32::from(s) << 16)
            .collect(),
        (hound::SampleFormat::Int, 24) => reader
            .samples::<i32>()
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|s| (s >> 8) << 8)
            .collect(),
        (hound::SampleFormat::Int, 32) => {
            reader.samples::<i32>().collect::<std::result::Result<Vec<_>, _>>()?
        }
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(|s| (s.clamp(-1.0, 1.0) * 2147483647.0) as i32)
            .collect(),
        _ => anyhow::bail!(
            "input WAV must be 16/24/32-bit integer or 32-bit float PCM, got {}-bit {:?}",
            spec.bits_per_sample,
            spec.sample_format
        ),
    };
    ensure!(!frames.is_empty(), "input WAV contains no audio");
    let channels = spec.channels as usize;
    let total = frames.len() / channels;
    ensure!(total > 0, "input WAV contains no audio");
    ensure!(
        total as u64 <= u64::from(source_rate) * MAX_SECONDS,
        "input WAV exceeds {MAX_SECONDS} seconds"
    );
    let left: Vec<f32> = (0..total).map(|i| frames[i * channels] as f32 / 2147483648.0).collect();
    let right: Vec<f32> = (0..total)
        .map(|i| frames[i * channels + channels.min(2) - 1] as f32 / 2147483648.0)
        .collect();
    ensure!(
        left.iter().chain(right.iter()).all(|s| s.is_finite()),
        "input WAV contains non-finite samples"
    );
    Ok((vec![left, right], source_rate))
}

/// Deterministic stereo test signal: one second of tone then one second of
/// silence, with distinct pitches per channel so cross-feed is observable.
fn simulated(index: u64, rate: u32) -> (f32, f32) {
    let position = index % u64::from(rate);
    let phase = position as f64 / f64::from(rate);
    let envelope = if phase < 0.5 {
        ((phase / 0.01).min(1.0) * ((0.5 - phase) / 0.01).min(1.0)).max(0.0)
    } else {
        0.0
    } as f32;
    let t = index as f64 / f64::from(rate);
    (
        0.2 * envelope * (std::f64::consts::TAU * 220.0 * t).sin() as f32,
        0.2 * envelope * (std::f64::consts::TAU * 330.0 * t).sin() as f32,
    )
}

type Ready = std::sync::mpsc::SyncSender<std::result::Result<(), String>>;

fn produce(
    kind: Kind,
    path: &Path,
    rate: u32,
    stop: Arc<AtomicBool>,
    state: &Arc<Mutex<State>>,
    ready: &Ready,
) -> Result<()> {
    if kind == Kind::Simulated {
        produce_simulated(rate, stop, state, ready)
    } else {
        produce_file(path, rate, stop, state, ready)
    }
}

fn produce_simulated(
    rate: u32,
    stop: Arc<AtomicBool>,
    state: &Arc<Mutex<State>>,
    ready: &Ready,
) -> Result<()> {
    let chunk = (rate / 100).max(1) as usize;
    state.lock().unwrap().metrics.input_rate = rate;
    let _ = ready.send(Ok(()));
    let mut index = 0u64;
    let frame_ns = 1_000_000_000u64 / u64::from(rate);
    while !stop.load(Ordering::Acquire) {
        let start = unix_ns();
        let mut frames = Vec::with_capacity(chunk);
        for _ in 0..chunk {
            let (left, right) = simulated(index, rate);
            frames.push(Frame {
                left,
                right,
                stamp: start + frames.len() as u64 * frame_ns,
            });
            index += 1;
        }
        {
            let mut shared = state.lock().unwrap();
            for frame in frames {
                shared.push_frame(frame);
            }
            shared.metrics.capture_frames += chunk as u64;
        }
        let next = Instant::now() + Duration::from_nanos(chunk as u64 * frame_ns);
        while Instant::now() < next && !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

fn produce_file(
    path: &Path,
    rate: u32,
    stop: Arc<AtomicBool>,
    state: &Arc<Mutex<State>>,
    ready: &Ready,
) -> Result<()> {
    let (channels, source_rate) = read_wav(path)?;
    let chunk = (source_rate / 100).max(16) as usize;
    let mut resampler = SincFixedIn::<f32>::new(
        rate as f64 / source_rate as f64,
        1.001,
        SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: 0.9,
            interpolation: SincInterpolationType::Cubic,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        },
        chunk,
        2,
    )?;
    let mut inputs = [VecDeque::<f32>::new(), VecDeque::<f32>::new()];
    let mut input_times = VecDeque::new();
    state.lock().unwrap().metrics.input_rate = source_rate;
    let _ = ready.send(Ok(()));
    let frame_ns = 1_000_000_000u64 / u64::from(rate);
    let mut cursor = 0usize;
    let mut produced = 0u64;
    while !stop.load(Ordering::Acquire) {
        let start = unix_ns();
        // The file loops: refill from the beginning whenever it runs out.
        while inputs[0].len() < chunk {
            if cursor >= channels[0].len() {
                cursor = 0;
            }
            let end = (cursor + chunk).min(channels[0].len());
            for (offset, index) in (cursor..end).enumerate() {
                inputs[0].push_back(channels[0][index]);
                inputs[1].push_back(channels[1][index]);
                input_times.push_back(start + (produced + offset as u64) * frame_ns);
            }
            produced += (end - cursor) as u64;
            cursor = end;
        }
        let input: Vec<Vec<f32>> = inputs.iter_mut().map(|q| q.drain(..chunk).collect()).collect();
        input_times.drain(..chunk);
        let first = *input_times.front().unwrap_or(&start);
        let delay = if source_rate == rate {
            0
        } else {
            resampler.output_delay() as u64 * frame_ns
        };
        let output = if source_rate == rate {
            input
        } else {
            resampler.process(&input, None)?
        };
        {
            let mut shared = state.lock().unwrap();
            for (i, (&left, &right)) in output[0].iter().zip(&output[1]).enumerate() {
                shared.push_frame(Frame {
                    left,
                    right,
                    stamp: first.saturating_sub(delay) + i as u64 * frame_ns,
                });
            }
            shared.metrics.capture_frames += output[0].len() as u64;
        }
        let paced = Duration::from_nanos(chunk as u64 * 1_000_000_000 / u64::from(source_rate));
        let next = Instant::now() + paced;
        while Instant::now() < next && !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const CAPACITY: usize = crate::rtp::RATE as usize * 60 / 1000;
    const TARGET: usize = crate::rtp::RATE as usize * 20 / 1000;

    fn source() -> StreamSource {
        let control =
            crate::equalizer::Control::new(crate::equalizer::Settings::default(), crate::rtp::RATE)
                .unwrap();
        StreamSource {
            state: Arc::new(Mutex::new(State {
                equalizer: crate::equalizer::Processor::new(control.initial(), crate::rtp::RATE),
                frames: VecDeque::new(),
                capacity: CAPACITY,
                target: TARGET,
                error: None,
                metrics: Metrics::default(),
                ages: VecDeque::new(),
            })),
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            equalizer_control: control,
        }
    }

    fn push(source: &StreamSource, left: f32, right: f32) {
        source.state.lock().unwrap().push_frame(Frame {
            left,
            right,
            stamp: unix_ns(),
        });
    }

    fn wav(directory: &Path, rate: u32, seconds: f32) -> std::path::PathBuf {
        let path = directory.join("input.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 2,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..(rate as f32 * seconds) as u32 {
            let value = ((i % 100) as i16 - 50) * 100;
            writer.write_sample(value).unwrap();
            writer.write_sample(-value).unwrap();
        }
        writer.finalize().unwrap();
        path
    }

    #[test]
    fn underrun_pads_silence_and_resumes_audio() {
        let source = source();
        assert!(source.packet(0.1).unwrap().0.iter().all(|b| *b == 0));
        assert_eq!(source.metrics().underrun_packets, 1);
        for _ in 0..FRAMES {
            push(&source, 0.01, 0.01);
        }
        assert!(source.packet(0.1).unwrap().0.iter().any(|b| *b != 0));
        assert_eq!(source.metrics().underrun_packets, 1);
    }

    #[test]
    fn equalizer_precedes_master_gain_and_pcm_conversion() {
        let source = source();
        let settings =
            crate::equalizer::Settings { enabled: true, preamp_db: -12.0, ..Default::default() };
        source
            .equalizer_control
            .set(1, crate::equalizer::Prepared::new(settings, crate::rtp::RATE).unwrap());
        for _ in 0..FRAMES * 3 {
            push(&source, 0.25, -0.25);
        }
        source.packet(0.5).unwrap();
        source.packet(0.5).unwrap();
        let (pcm, marker) = source.packet(0.5).unwrap();
        let last = &pcm[pcm.len() - 4..];
        let expected = (0.25 * 10f32.powf(-12.0 / 20.0) * 0.5 * 32767.0).round() as i16;
        assert_eq!(i16::from_be_bytes([last[0], last[1]]), expected);
        assert_eq!(i16::from_be_bytes([last[2], last[3]]), -expected);
        assert!(marker.is_some());
    }

    #[test]
    fn overflow_and_scheduler_recovery_drop_old_frames_without_failure() {
        let source = source();
        {
            let mut s = source.state.lock().unwrap();
            for _ in 0..CAPACITY + 100 {
                s.push_frame(Frame {
                    left: 0.01,
                    right: 0.01,
                    stamp: 0,
                });
            }
            assert_eq!(s.frames.len(), CAPACITY);
        }
        assert_eq!(source.metrics().dropped_frames, 100);
        source.discard_stale();
        assert_eq!(source.metrics().dropped_frames, (CAPACITY + 100) as u64);
        assert!(source.packet(0.1).unwrap().0.iter().all(|b| *b == 0));
        assert_eq!(source.metrics().underrun_packets, 1);
    }

    #[test]
    fn file_decoding_rejects_bad_formats() {
        let directory = std::env::temp_dir().join(format!("airflash-input-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut file = std::fs::File::create(directory.join("text.wav")).unwrap();
        file.write_all(b"not a wave file").unwrap();
        assert!(read_wav(&directory.join("text.wav")).is_err());
        assert!(read_wav(&directory.join("missing.wav")).is_err());
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn file_source_streams_and_loops() {
        let directory = std::env::temp_dir().join(format!("airflash-input-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = wav(&directory, crate::rtp::RATE, 0.5);
        let control =
            crate::equalizer::Control::new(crate::equalizer::Settings::default(), crate::rtp::RATE)
                .unwrap();
        let source = StreamSource::start(Kind::File, &path, crate::rtp::RATE, control).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !source.ready().unwrap() {
            assert!(std::time::Instant::now() < deadline, "file source never became ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(source.metrics().input_rate, crate::rtp::RATE);
        let (pcm, marker) = source.packet(1.0).unwrap();
        assert!(pcm.iter().any(|b| *b != 0), "file source emitted silence");
        assert!(marker.is_some());
        // The file is 0.5 s long and loops, so audio continues past its end.
        std::thread::sleep(Duration::from_millis(700));
        let (pcm, _) = source.packet(1.0).unwrap();
        assert!(pcm.iter().any(|b| *b != 0), "file source did not loop");
        drop(source);
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn file_source_resamples_other_rates() {
        let directory = std::env::temp_dir().join(format!("airflash-resample-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = wav(&directory, 48000, 0.5);
        let control =
            crate::equalizer::Control::new(crate::equalizer::Settings::default(), 44100).unwrap();
        let source = StreamSource::start(Kind::File, &path, 44100, control).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !source.ready().unwrap() {
            assert!(std::time::Instant::now() < deadline, "resampled source never became ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(source.metrics().input_rate, 48000);
        let (pcm, _) = source.packet(1.0).unwrap();
        assert_eq!(pcm.len(), FRAMES * 4);
        assert!(pcm.iter().any(|b| *b != 0));
        drop(source);
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn simulated_source_is_deterministic_and_matches_kind_parsing() {
        assert_eq!(Kind::parse("file"), Some(Kind::File));
        assert_eq!(Kind::parse("simulated"), Some(Kind::Simulated));
        assert_eq!(Kind::parse("loopback"), None);
        let control =
            crate::equalizer::Control::new(crate::equalizer::Settings::default(), crate::rtp::RATE)
                .unwrap();
        let source = StreamSource::start(
            Kind::Simulated,
            Path::new(""),
            crate::rtp::RATE,
            control,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !source.ready().unwrap() {
            assert!(std::time::Instant::now() < deadline, "simulated source never became ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        let (first, _) = source.packet(1.0).unwrap();
        drop(source);
        let control =
            crate::equalizer::Control::new(crate::equalizer::Settings::default(), crate::rtp::RATE)
                .unwrap();
        let repeat = StreamSource::start(
            Kind::Simulated,
            Path::new(""),
            crate::rtp::RATE,
            control,
        )
        .unwrap();
        while !repeat.ready().unwrap() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let (second, _) = repeat.packet(1.0).unwrap();
        assert_eq!(first, second, "simulated input must be reproducible");
        assert!(first.iter().any(|b| *b != 0));
    }
}
