use anyhow::Result;
use serde::Serialize;

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

pub struct Loopback;

impl Loopback {
    pub fn start(
        _endpoint: Option<String>,
        _rate: u32,
        _equalizer: crate::equalizer::Control,
    ) -> Result<Self> {
        anyhow::bail!(
            "loopback source is currently only available on Windows WASAPI; on Linux use source=wav in this release"
        );
    }

    pub fn ready(&self) -> Result<bool> {
        let _ = self;
        Ok(false)
    }

    pub fn packet(&self, _gain: f32) -> Result<(Vec<u8>, Option<u64>)> {
        let _ = self;
        anyhow::bail!("loopback source unavailable on this platform")
    }

    pub fn discard_stale(&self) {
        let _ = self;
    }

    pub fn metrics(&self) -> Metrics {
        Metrics::default()
    }
}
