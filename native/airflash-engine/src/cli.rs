//! Headless daemon lifecycle for the `airflash-cli` binary: runtime directory,
//! state file, a Unix stream control channel and graceful signal handling.
//!
//! Only `std` plus `libc` for signals and process liveness are used, so the
//! daemon behaves the same under systemd --user, ssh without a tty, or in a
//! container. Streaming reuses `session::probe_with_controls`; no AirPlay
//! protocol code is duplicated here.
use crate::session::ProbeOptions;
use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{OpenOptions, Permissions},
    io::{self, BufRead, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long `start --daemon` waits for the child to leave the starting phase.
const READY_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the foreground loop checks stop requests and worker completion.
const POLL: Duration = Duration::from_millis(100);

/// Runtime directory: explicit override, then the XDG runtime dir, then a
/// per-user state directory. Never a world-writable location.
pub fn runtime_dir() -> PathBuf {
    runtime_dir_from(
        std::env::var_os("AIRFLASH_RUNTIME_DIR").map(PathBuf::from),
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// Pure precedence rules for the runtime directory, testable without env vars.
pub fn runtime_dir_from(
    explicit: Option<PathBuf>,
    xdg_runtime: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Some(path) = xdg_runtime {
        return path.join("airflash");
    }
    home.unwrap_or_else(|| PathBuf::from("."))
        .join(".local/state/airflash")
}

/// Resolved file locations for one daemon instance.
#[derive(Clone)]
pub struct Layout {
    root: PathBuf,
}
impl Layout {
    pub fn resolve() -> Self {
        Self { root: runtime_dir() }
    }
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn state(&self) -> PathBuf {
        self.root.join("session.json")
    }
    pub fn log(&self) -> PathBuf {
        self.root.join("session.log")
    }
    pub fn socket(&self) -> PathBuf {
        self.root.join("control.sock")
    }
    pub fn lock(&self) -> PathBuf {
        self.root.join("daemon.lock")
    }
    pub fn create(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create runtime directory {}", self.root.display()))?;
        let _ = OpenOptions::new();
        std::fs::set_permissions(&self.root, Permissions::from_mode(0o700))?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Starting,
    Streaming,
    Stopped,
    Failed,
}

/// On-disk daemon state. `pid` is what `stop` signals; it is written by the
/// daemon itself so the parent never guesses a child pid.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub pid: u32,
    pub status: Status,
    pub host: String,
    pub port: u16,
    pub source: String,
    pub sample_rate: u32,
    pub started_unix_ms: u128,
    pub log_path: PathBuf,
    pub control_socket: PathBuf,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub metrics: Value,
}

impl State {
    pub fn read(layout: &Layout) -> Result<Option<Self>> {
        let path = layout.state();
        let data = match std::fs::read(&path) {
            Ok(data) => data,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let state: Self = serde_json::from_slice(&data)
            .with_context(|| format!("parse {}", path.display()))?;
        Ok(Some(state))
    }
    /// Atomic replace keeps readers from ever seeing a partial document.
    pub fn write(&self, layout: &Layout) -> Result<()> {
        layout.create()?;
        let target = layout.state();
        let temp = layout.root().join(format!("session.json.{}", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .with_context(|| format!("open {}", temp.display()))?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &target)
            .with_context(|| format!("replace {}", target.display()))?;
        Ok(())
    }
    pub fn age_ms(&self) -> u128 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        now.saturating_sub(self.started_unix_ms)
    }
}

fn open_private(path: &Path, append: bool) -> Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).mode(0o600);
    if append {
        options.append(true);
    } else {
        options.truncate(true);
    }
    options.open(path).with_context(|| format!("open {}", path.display()))
}

/// Append one JSON event line to the daemon log.
pub fn append_log(path: &Path, event: &Value) -> Result<()> {
    let mut file = open_private(path, true)?;
    writeln!(file, "{event}")?;
    Ok(())
}

/// Last `lines` lines of a log file, oldest first.
pub fn tail(path: &Path, lines: usize) -> Result<Vec<String>> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut out: Vec<String> = String::from_utf8_lossy(&data)
        .lines()
        .map(str::to_owned)
        .collect();
    let start = out.len().saturating_sub(lines);
    out.drain(..start);
    Ok(out)
}

/// True when the process exists (EPERM means it exists but is not ours).
/// `kill(-1, 0)` broadcasts to every process, so pids that do not fit in a
/// signed pid_t are reported dead instead of accidentally signalling groups.
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Ask a process to terminate gracefully (RAII teardown paths run first).
pub fn request_stop(pid: u32) -> Result<()> {
    ensure!(valid_pid(pid), "invalid pid");
    let result = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    ensure!(result == 0, "cannot signal pid {pid}");
    Ok(())
}

/// Last resort after a graceful stop times out.
pub fn force_stop(pid: u32) -> Result<()> {
    ensure!(valid_pid(pid), "invalid pid");
    let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    ensure!(result == 0, "cannot signal pid {pid}");
    Ok(())
}

/// A pid that can be passed to `kill` without wrapping negative.
fn valid_pid(pid: u32) -> bool {
    pid > 0 && pid <= i32::MAX as u32
}

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn on_signal(_signal: i32) {
    STOP.store(true, Ordering::Release);
}
/// Route SIGTERM/SIGINT/SIGHUP into the graceful cancel path so RTSP teardown,
/// credential flush and final metrics still happen.
pub fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGHUP, on_signal as *const () as usize);
    }
}
pub fn requested_stop() -> bool {
    STOP.load(Ordering::Acquire)
}

/// Shared playback controls handed to the running session.
#[derive(Clone)]
pub struct Controls {
    pub volume: crate::volume::Control,
    pub gain: Arc<AtomicU32>,
    pub equalizer: crate::equalizer::Control,
}

/// Serve `{"command":...}` requests on a Unix stream socket until `stop` is set.
/// One short-lived connection per request keeps the daemon single threaded and
/// makes a stalled client unable to block playback control.
pub fn serve_control(
    path: &Path,
    controls: Controls,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>> {
    let path = path.to_path_buf();
    let _ = std::fs::remove_file(&path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = std::os::unix::net::UnixListener::bind(&path)
        .with_context(|| format!("bind {}", path.display()))?;
    listener
        .set_nonblocking(true)
        .context("configure control listener")?;
    let worker = thread::Builder::new()
        .name("airflash-control".into())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let controls = controls.clone();
                        thread::Builder::new()
                            .name("airflash-control-io".into())
                            .spawn(move || {
                                let _ = answer(stream, &controls);
                            })
                            .ok();
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(POLL);
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = std::fs::remove_file(&path);
        })?;
    Ok(worker)
}

fn answer(
    stream: std::os::unix::net::UnixStream,
    controls: &Controls,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = io::BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    let read = reader
        .read_line(&mut request)
        .context("read control request")?;
    ensure!(read > 0 && request.len() <= 4096, "empty or oversized control request");
    let mut reply = handle_control(request.trim().as_bytes(), controls);
    reply.push('\n');
    let mut stream = reader.into_inner();
    stream.write_all(reply.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn handle_control(request: &[u8], controls: &Controls) -> String {
    let parsed: std::result::Result<Value, _> = serde_json::from_slice(request);
    let Ok(request) = parsed else {
        return json!({"ok":false,"error":"invalid control request"}).to_string();
    };
    let command = request.get("command").and_then(Value::as_str).unwrap_or("");
    match command {
        "volume" => {
            let percent = request.get("percent").and_then(Value::as_u64);
            let sequence = request.get("sequence").and_then(Value::as_u64);
            match (percent, sequence) {
                (Some(percent), Some(sequence)) if percent <= 100 && sequence > 0 => {
                    controls.volume.set(crate::volume::Command {
                        sequence,
                        percent: percent as u8,
                    });
                    json!({"ok":true,"command":"volume"}).to_string()
                }
                _ => json!({"ok":false,"error":"volume needs percent 0..100 and a positive sequence"})
                    .to_string(),
            }
        }
        "gain" => {
            let gain = request.get("value").and_then(Value::as_f64);
            match gain.filter(|g| g.is_finite() && (0.0..=1.0).contains(g)) {
                Some(gain) => {
                    controls
                        .gain
                        .store((gain as f32).to_bits(), Ordering::Release);
                    json!({"ok":true,"command":"gain"}).to_string()
                }
                None => json!({"ok":false,"error":"gain must be 0..1"}).to_string(),
            }
        }
        "status" => {
            let gain = f32::from_bits(controls.gain.load(Ordering::Acquire));
            let equalizer = controls.equalizer.latest();
            json!({"ok":true,"command":"status","gain":gain,"equalizer_sequence":equalizer.map(|(s,_)| s)})
                .to_string()
        }
        _ => json!({"ok":false,"error":"unknown control command"}).to_string(),
    }
}

/// Send one request to a running daemon and wait for its reply.
pub fn request_control(path: &Path, request: &Value) -> Result<Value> {
    let mut stream = std::os::unix::net::UnixStream::connect(path)
        .with_context(|| format!("connect to {}", path.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .context("set control timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .context("set control timeout")?;
    let mut payload = request.to_string();
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .context("send control request")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("finish control request")?;
    let mut reply = String::new();
    io::BufReader::new(stream)
        .read_line(&mut reply)
        .context("read control reply")?;
    ensure!(!reply.trim().is_empty(), "daemon closed the control channel");
    let reply: Value = serde_json::from_str(reply.trim()).context("parse control reply")?;
    Ok(reply)
}

/// Prevent two daemons on one runtime directory. A leftover lock from a killed
/// process is reclaimed once its pid is gone.
#[derive(Debug)]
pub struct DaemonLock {
    path: PathBuf,
}
impl DaemonLock {
    pub fn acquire(layout: &Layout) -> Result<Self> {
        layout.create()?;
        let path = layout.lock();
        for attempt in 0..2 {
            match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let pid = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|text| text.trim().parse::<u32>().ok())
                        .unwrap_or(0);
                    if attempt == 0 && process_alive(pid) {
                        anyhow::bail!("another AirFlash daemon is running (pid {pid})");
                    }
                    if attempt == 0 {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    anyhow::bail!("cannot acquire {}", path.display());
                }
                Err(e) => return Err(e).with_context(|| format!("open {}", path.display())),
            }
        }
        unreachable!()
    }
}
impl Drop for DaemonLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Shared state the worker thread updates as session events arrive.
struct Shared {
    state: State,
    layout: Layout,
}
impl Shared {
    fn event(&mut self, event: &Value) {
        let status = event.get("event").and_then(Value::as_str).unwrap_or("");
        match status {
            "streaming" => {
                self.state.status = Status::Streaming;
                self.state.metrics = event
                    .get("members")
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            "error" => {
                self.state.status = Status::Failed;
                self.state.error = event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            "stopped" => self.state.status = Status::Stopped,
            _ => return,
        }
        let (state, layout) = (&self.state, &self.layout);
        if let Err(e) = state.write(layout) {
            eprintln!("airflash-cli: cannot update state: {e:#}");
        }
    }
}

/// One foreground daemon run: state file, control socket, streaming worker and
/// graceful shutdown. Returns the process exit code.
pub fn run_foreground(
    options: ProbeOptions,
    layout: Layout,
    log: Option<PathBuf>,
    quiet: bool,
) -> Result<i32> {
    let _lock = DaemonLock::acquire(&layout)?;
    let log_path = log.unwrap_or_else(|| layout.log());
    let state = State {
        version: 1,
        pid: std::process::id(),
        status: Status::Starting,
        host: options
            .peers
            .first()
            .map(|p| p.host.to_string())
            .unwrap_or_default(),
        port: options.peers.first().map_or(7000, |p| p.port),
        source: options.source.clone(),
        sample_rate: options.sample_rate,
        started_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
        log_path: log_path.clone(),
        control_socket: layout.socket(),
        error: None,
        metrics: Value::Null,
    };
    state.write(&layout)?;
    if let Err(e) = append_log(
        &log_path,
        &json!({"event":"daemon_start","pid":state.pid,"host":state.host,"port":state.port,"source":state.source,"sample_rate":state.sample_rate}),
    ) {
        eprintln!("airflash-cli: cannot write log: {e:#}");
    }
    install_signal_handlers();
    let controls = Controls {
        volume: crate::volume::Control::default(),
        gain: Arc::new(AtomicU32::new(options.gain.to_bits())),
        equalizer: crate::equalizer::Control::new(options.equalizer, options.sample_rate)?,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let control_socket = layout.socket();
    let control_worker = serve_control(&control_socket, controls.clone(), stop.clone())?;
    let shared = Arc::new(Mutex::new(Shared { state, layout: layout.clone() }));
    let log_worker = log_path.clone();
    let emit_shared = shared.clone();
    let emit = move |event: Value| {
        if let Err(e) = append_log(&log_worker, &event) {
            eprintln!("airflash-cli: cannot write log: {e:#}");
        }
        if !quiet {
            println!("{event}");
            let _ = io::stdout().flush();
        }
        emit_shared.lock().unwrap().event(&event);
    };
    let cancel = crate::rtsp::Cancellation::default();
    let worker_cancel = cancel.clone();
    let (done, finished) = std::sync::mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("airflash-session".into())
        .spawn(move || {
            let result = crate::session::probe_with_controls(
                options,
                worker_cancel,
                controls.gain,
                controls.volume,
                controls.equalizer,
                emit,
            );
            let _ = done.send(result);
        })?;
    let outcome = loop {
        if requested_stop() {
            cancel.cancel();
            let _ = worker.join();
            break Ok(());
        }
        match finished.try_recv() {
            Ok(result) => {
                cancel.cancel();
                let _ = worker.join();
                break result;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => thread::sleep(POLL),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                break Err(anyhow!("session worker exited unexpectedly"))
            }
        }
    };
    stop.store(true, Ordering::Release);
    let _ = control_worker.join();
    {
        let mut shared = shared.lock().unwrap();
        let Shared { state, layout } = &mut *shared;
        if matches!(state.status, Status::Starting | Status::Streaming) {
            state.status = Status::Stopped;
        }
        if let Err(error) = &outcome {
            if state.error.is_none() {
                state.error = Some(format!("{error:#}"));
            }
            if state.status != Status::Failed {
                state.status = Status::Failed;
            }
        }
        if let Err(e) = state.write(layout) {
            eprintln!("airflash-cli: cannot update state: {e:#}");
        }
    }
    let _ = std::fs::remove_file(&control_socket);
    match outcome {
        Ok(()) => Ok(0),
        Err(error) => {
            eprintln!("airflash-cli: {error:#}");
            Ok(1)
        }
    }
}

/// Spawn a detached daemon and wait until it leaves the starting phase. The
/// child's stdout/stderr are appended to `log`, the same file the daemon writes
/// its events to, so there is never a second log to chase.
pub fn spawn_daemon(
    exe: &Path,
    args: &[std::ffi::OsString],
    layout: &Layout,
    log: &Path,
) -> Result<u32> {
    layout.create()?;
    let stdout = open_private(log, true)?;
    let stderr = stdout.try_clone()?;
    let child = std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("spawn daemon process")?;
    let pid = child.id();
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        match State::read(layout)? {
            Some(state) if state.pid == pid && state.status != Status::Starting => {
                if state.status == Status::Failed {
                    anyhow::bail!(
                        "daemon failed on {}:{}: {}",
                        state.host,
                        state.port,
                        state.error.as_deref().unwrap_or("unknown failure")
                    );
                }
                return Ok(pid);
            }
            _ => {}
        }
        // A child that died before reporting is a failure, not a timeout.
        if !process_alive(pid) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("daemon did not report readiness within {READY_TIMEOUT:?}")
}

impl Status {
    pub fn status_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Streaming => "streaming",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

/// Wait for a pid to disappear, escalating to SIGKILL after `grace`.
pub fn wait_for_exit(pid: u32, grace: Duration, force: bool) -> Result<bool> {
    let deadline = Instant::now() + grace;
    while process_alive(pid) {
        if Instant::now() >= deadline {
            if force {
                force_stop(pid)?;
                let hard = Instant::now() + Duration::from_secs(3);
                while process_alive(pid) && Instant::now() < hard {
                    thread::sleep(Duration::from_millis(50));
                }
                return Ok(!process_alive(pid));
            }
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(label: &str) -> (Layout, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "airflash-cli-unit-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        (Layout::with_root(&root), root)
    }

    fn sample_state(layout: &Layout) -> State {
        State {
            version: 1,
            pid: std::process::id(),
            status: Status::Streaming,
            host: "192.168.1.10".into(),
            port: 7000,
            source: "simulated".into(),
            sample_rate: 44100,
            started_unix_ms: 1_700_000_000_000,
            log_path: layout.log(),
            control_socket: layout.socket(),
            error: None,
            metrics: Value::Null,
        }
    }

    #[test]
    fn runtime_directory_precedence_is_explicit_then_xdg_then_home() {
        assert_eq!(
            runtime_dir_from(Some(PathBuf::from("/tmp/a")), Some(PathBuf::from("/run/b")), Some(PathBuf::from("/home/c"))),
            PathBuf::from("/tmp/a")
        );
        assert_eq!(
            runtime_dir_from(None, Some(PathBuf::from("/run/b")), Some(PathBuf::from("/home/c"))),
            PathBuf::from("/run/b/airflash")
        );
        assert_eq!(
            runtime_dir_from(None, None, Some(PathBuf::from("/home/c"))),
            PathBuf::from("/home/c/.local/state/airflash")
        );
        assert_eq!(runtime_dir_from(None, None, None), PathBuf::from("./.local/state/airflash"));
    }

    #[test]
    fn state_round_trip_is_atomic_and_reports_absence() {
        let (layout, root) = layout("state");
        assert!(State::read(&layout).unwrap().is_none(), "missing state must read as none");
        let state = sample_state(&layout);
        state.write(&layout).unwrap();
        let read = State::read(&layout).unwrap().expect("state written");
        assert_eq!(read.pid, state.pid);
        assert_eq!(read.status, Status::Streaming);
        assert_eq!(read.port, 7000);
        let mode = std::fs::metadata(root.join("session.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "state file must stay private");
        // No temporary files survive a replace.
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp") || name.starts_with("session.json."))
            .collect();
        assert!(leftovers.is_empty(), "temporary state files left: {leftovers:?}");
        std::fs::write(layout.state(), b"{not json").unwrap();
        assert!(State::read(&layout).is_err(), "corrupt state must be an error");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn log_append_and_tail_return_oldest_first() {
        let (layout, root) = layout("log");
        let path = layout.log();
        append_log(&path, &json!({"event":"daemon_start"})).unwrap();
        append_log(&path, &json!({"event":"streaming"})).unwrap();
        append_log(&path, &json!({"event":"stopped"})).unwrap();
        assert_eq!(tail(&path, 2).unwrap().len(), 2);
        let lines = tail(&path, 10).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("daemon_start"));
        assert!(lines[2].contains("stopped"));
        assert!(tail(&path, 0).unwrap().is_empty());
        assert!(tail(&root.join("missing.log"), 1).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn process_liveness_distinguishes_live_and_dead_pids() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(0));
        assert!(!process_alive(u32::MAX));
    }

    #[test]
    fn daemon_lock_detects_a_live_process_and_reclaims_a_stale_file() {
        let (layout, root) = layout("lock");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(layout.lock(), format!("{}", std::process::id())).unwrap();
        let error = DaemonLock::acquire(&layout).unwrap_err().to_string();
        assert!(error.contains("another AirFlash daemon"), "unexpected error: {error}");
        std::fs::write(layout.lock(), format!("{}", u32::MAX)).unwrap();
        let guard = DaemonLock::acquire(&layout).expect("stale lock is reclaimed");
        assert_eq!(
            std::fs::read_to_string(layout.lock()).unwrap().trim(),
            std::process::id().to_string()
        );
        drop(guard);
        assert!(!layout.lock().exists(), "lock file must be removed on drop");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn control_channel_round_trips_commands_and_rejects_bad_ones() {
        let (layout, root) = layout("control");
        let controls = Controls {
            volume: crate::volume::Control::default(),
            gain: Arc::new(AtomicU32::new(0.5f32.to_bits())),
            equalizer: crate::equalizer::Control::new(
                crate::equalizer::Settings::default(),
                44100,
            )
            .unwrap(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let socket = layout.socket();
        let worker = serve_control(&socket, controls.clone(), stop.clone()).unwrap();
        // The listener is nonblocking, so give it a moment to accept.
        thread::sleep(Duration::from_millis(50));
        let reply = request_control(&socket, &json!({"command":"volume","percent":30,"sequence":4})).unwrap();
        assert_eq!(reply["ok"], json!(true));
        let reply = request_control(&socket, &json!({"command":"gain","value":0.25})).unwrap();
        assert_eq!(reply["ok"], json!(true));
        // A live equalizer update is visible through the status command.
        let prepared = crate::equalizer::Prepared::new(
            crate::equalizer::Settings::default(),
            44100,
        )
        .unwrap();
        controls.equalizer.set(7, prepared);
        let reply = request_control(&socket, &json!({"command":"status"})).unwrap();
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["equalizer_sequence"], json!(7));
        for bad in [
            json!({"command":"volume","percent":101,"sequence":1}),
            json!({"command":"volume","percent":10,"sequence":0}),
            json!({"command":"gain","value":1.5}),
            json!({"command":"nope"}),
            json!({"command":"volume"}),
        ] {
            let reply = request_control(&socket, &bad).unwrap();
            assert_eq!(reply["ok"], json!(false), "{bad} must be rejected");
        }
        stop.store(true, Ordering::Release);
        worker.join().unwrap();
        assert!(request_control(&socket, &json!({"command":"status"})).is_err());
        assert!(!socket.exists(), "socket must be removed when the server stops");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn wait_for_exit_returns_immediately_for_dead_processes() {
        assert!(wait_for_exit(u32::MAX, Duration::from_millis(10), false).unwrap());
        assert!(wait_for_exit(0, Duration::from_millis(10), false).unwrap());
    }
}
