//! Headless, non-interactive control surface for the AirPlay sender.
//!
//! Every command works without a graphical desktop and without a tty:
//! `discover`, `start` (foreground or detached daemon), `stop`, `status`,
//! `logs`, `volume`, `gain` and `pair`. The streaming path reuses the engine's
//! `session::probe_with_controls`; no AirPlay protocol code lives here.
#![cfg(all(unix, feature = "cli"))]

use airflash_engine::{
    cli::{self, Layout, State, Status},
    discovery, rtsp::Cancellation,
    session::ProbeOptions,
    source::Kind,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    net::IpAddr,
    path::PathBuf,
    process::ExitCode,
    time::Duration,
};

const USAGE: &str = "\
airflash-cli - headless AirPlay 2 sender control

usage:
  airflash-cli discover [--timeout-ms N] [--json] [--service airplay|raop|all]
  airflash-cli start --host IP [--port N] [--source file|simulated|loopback]
                    [--wav PATH] [--rate N] [--latency-ms N] [--gain F]
                    [--timing ptp|ntp|auto] [--codec alac|pcm|auto] [--monitor NAME]
                    [--config PATH] [--daemon|--foreground]
                    [--log PATH] [--runtime-dir PATH] [--quiet]
  airflash-cli start --device NAME   (resolve the receiver through mDNS first)
  airflash-cli stop [--timeout-ms N] [--force]
  airflash-cli status [--json]
  airflash-cli logs [--lines N]
  airflash-cli volume --percent N --sequence N
  airflash-cli gain --value F
  airflash-cli pair --host IP [--port N] [--pin N]
  airflash-cli version

Without --daemon, `start` runs in the foreground and prints one JSON event per
line (journal-friendly). With --daemon it detaches, logs to the session log and
returns once the session is streaming.

environment:
  AIRFLASH_RUNTIME_DIR   state/log/control-socket directory
                         (default: $XDG_RUNTIME_DIR/airflash, then
                          ~/.local/state/airflash)
";

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
struct SessionSettings {
    host: Option<String>,
    device: Option<String>,
    port: Option<u16>,
    source: Option<String>,
    wav: Option<PathBuf>,
    rate: Option<u32>,
    latency_ms: Option<u32>,
    gain: Option<f32>,
    timing: Option<String>,
    codec: Option<String>,
    monitor: Option<String>,
    log: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    /// Receiver-advertised RAOP codecs, filled by mDNS resolution (never from
    /// the JSON config file). Empty means unknown: the engine prefers ALAC and
    /// falls back to PCM when the audio SETUP is rejected.
    #[serde(default, skip_serializing)]
    codecs: Vec<u8>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("airflash-cli: {error:#}");
            if error.to_string().starts_with("usage") {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String]) -> Result<u8> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    let rest = &args[1.min(args.len())..];
    match command {
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(0)
        }
        "version" | "--version" => {
            println!(
                "airflash-cli {} (AirFlash headless Linux slice)",
                env!("CARGO_PKG_VERSION")
            );
            Ok(0)
        }
        "discover" => discover(rest),
        "start" => start(rest),
        "stop" => stop(rest),
        "status" => status(rest),
        "logs" => logs(rest),
        "volume" | "gain" => control(command, rest),
        "pair" => pair(rest),
        _ => {
            eprintln!("airflash-cli: unknown command '{command}'");
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

/// Parsed options: `--name value` / `--name=value` pairs plus standalone
/// boolean flags. Unknown names are errors so typos never become defaults.
struct Parsed {
    values: HashMap<String, String>,
    flags: std::collections::HashSet<String>,
}
impl Parsed {
    fn parse(args: &[String], valued: &[&str], boolean: &[&str]) -> Result<Self> {
        let mut values: HashMap<String, String> = HashMap::new();
        let mut flags = std::collections::HashSet::new();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            ensure!(arg.starts_with("--"), "usage: expected an option, got '{arg}'");
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) => (flag, Some(value.to_string())),
                None => (arg.as_str(), None),
            };
            let name = flag.trim_start_matches("--");
            if boolean.contains(&name) {
                ensure!(inline.is_none(), "usage: --{name} takes no value");
                flags.insert(name.to_string());
                index += 1;
                continue;
            }
            ensure!(valued.contains(&name), "usage: unknown option --{name}");
            let value = match inline {
                Some(value) => value,
                None => {
                    let next = args
                        .get(index + 1)
                        .filter(|value| !value.starts_with("--"))
                        .cloned()
                        .ok_or_else(|| anyhow!("usage: --{name} needs a value"))?;
                    index += 1;
                    next
                }
            };
            values.insert(name.to_string(), value);
            index += 1;
        }
        Ok(Self { values, flags })
    }
    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
    fn text(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>> {
        match self.values.get(name) {
            Some(value) => value
                .parse::<T>()
                .map(Some)
                .map_err(|_| anyhow!("usage: --{name} needs a number, got '{value}'")),
            None => Ok(None),
        }
    }
}

const START_VALUED: &[&str] = &[
    "host", "device", "port", "source", "wav", "rate", "latency-ms", "gain", "timing", "codec",
    "monitor", "config", "log", "runtime-dir", "timeout-ms",
];
const START_BOOLEAN: &[&str] = &["daemon", "foreground", "quiet"];

fn settings(args: &[String]) -> Result<(SessionSettings, Parsed, bool, bool)> {
    let parsed = Parsed::parse(args, START_VALUED, START_BOOLEAN)?;
    let (daemon, quiet) = (parsed.flag("daemon"), parsed.flag("quiet"));
    let mut settings = SessionSettings::default();
    if let Some(path) = parsed.text("config") {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read config {}", PathBuf::from(path).display()))?;
        settings = serde_json::from_str(&text)
            .with_context(|| format!("parse config {}", PathBuf::from(path).display()))?;
    }
    // Explicit flags win over the config file.
    for (name, value) in parsed.values.iter() {
        match name.as_str() {
            "host" => settings.host = Some(value.clone()),
            "device" => settings.device = Some(value.clone()),
            "port" => settings.port = Some(value.parse()?),
            "source" => settings.source = Some(value.clone()),
            "wav" => settings.wav = Some(PathBuf::from(value)),
            "rate" => settings.rate = Some(value.parse()?),
            "latency-ms" => settings.latency_ms = Some(value.parse()?),
            "gain" => settings.gain = Some(value.parse()?),
            "timing" => settings.timing = Some(value.clone()),
            "codec" => settings.codec = Some(value.clone()),
            "monitor" => settings.monitor = Some(value.clone()),
            "log" => settings.log = Some(PathBuf::from(value)),
            "runtime-dir" => settings.runtime_dir = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    Ok((settings, parsed, daemon, quiet))
}

fn layout(settings: &SessionSettings, parsed: &Parsed) -> Layout {
    if let Some(path) = settings.runtime_dir.clone() {
        return Layout::with_root(path);
    }
    if let Some(path) = parsed.text("runtime-dir") {
        return Layout::with_root(PathBuf::from(path));
    }
    Layout::resolve()
}

fn resolve_host(settings: &mut SessionSettings, timeout_ms: u64) -> Result<()> {
    let device = match &settings.device {
        Some(device) => device.clone(),
        None => return Ok(()),
    };
    let services = discovery::discover(
        &[discovery::AIRPLAY_SERVICE, discovery::RAOP_SERVICE],
        Duration::from_millis(timeout_ms),
        discovery_target(),
    )?;
    let mut seen = std::collections::HashSet::new();
    let matches: Vec<_> = services
        .iter()
        .filter(|service| {
            service.instance == device
                || service.instance.contains(&device)
                || service.host == device
                || service.addresses.iter().any(|a| a.to_string() == device)
        })
        // One receiver answers on both _airplay and _raop; they are the same
        // endpoint and must not count as ambiguous.
        .filter(|service| seen.insert((service.addresses.first().copied(), service.port)))
        .collect();
    match matches.as_slice() {
        [] => bail!("no discovered receiver matches '{device}'"),
        [service] => {
            let host = service
                .addresses
                .first()
                .copied()
                .ok_or_else(|| anyhow!("receiver '{device}' has no IPv4 address"))?;
            settings.host = Some(host.to_string());
            settings.port = Some(service.port);
            settings.device = None;
            // The RAOP TXT record advertises supported codecs as `cn=0,1,2,3`
            // (0 PCM, 1 ALAC). The engine prefers ALAC and falls back, but an
            // explicit receiver advertisement still beats guessing.
            if let Some(cn) = service.txt.get("cn") {
                settings.codecs = cn
                    .split(',')
                    .filter_map(|item| item.trim().parse::<u8>().ok())
                    .collect();
            }
            Ok(())
        }
        _ => bail!("'{device}' matches {} receivers; use --host instead", matches.len()),
    }
}

fn discovery_target() -> std::net::SocketAddr {
    let (octets, port) = discovery::DEFAULT_MULTICAST;
    std::net::SocketAddr::from((octets, port))
}

fn build_options(settings: &SessionSettings) -> Result<ProbeOptions> {
    let host = settings
        .host
        .as_deref()
        .ok_or_else(|| anyhow!("usage: --host IP or --device NAME is required"))?;
    let host: IpAddr = host
        .parse()
        .map_err(|_| anyhow!("usage: --host must be an IPv4 address, got '{host}'"))?;
    ensure!(host.is_ipv4(), "usage: --host must be IPv4");
    let source = settings.source.as_deref().unwrap_or("simulated");
    ensure!(
        source == "loopback" || Kind::parse(source).is_some(),
        "usage: --source must be file, simulated or loopback"
    );
    let wav_path = match source {
        "file" => settings
            .wav
            .clone()
            .ok_or_else(|| anyhow!("usage: --source file needs --wav PATH"))?,
        _ => settings.wav.clone().unwrap_or_default(),
    };
    let rate = settings.rate.unwrap_or(44100);
    ensure!(rate == 44100 || rate == 48000, "usage: --rate must be 44100 or 48000");
    let timing = settings.timing.as_deref().unwrap_or("auto").to_string();
    ensure!(
        ["ptp", "ntp", "auto"].contains(&timing.as_str()),
        "usage: --timing must be ptp, ntp or auto"
    );
    let codec = settings.codec.as_deref().unwrap_or("auto");
    let codecs = match codec {
        "alac" => vec![1],
        "pcm" => vec![0],
        "auto" => settings.codecs.clone(),
        _ => bail!("usage: --codec must be alac, pcm or auto, got '{codec}'"),
    };
    let options = ProbeOptions {
        peers: vec![airflash_engine::session::Peer {
            host,
            port: settings.port.unwrap_or(7000),
            codecs,
        }],
        wav_path,
        source: source.to_string(),
        capture_endpoint: settings.monitor.clone(),
        duration_ms: 0,
        latency_ms: settings.latency_ms.unwrap_or(200),
        gain: settings.gain.unwrap_or(1.0),
        equalizer: airflash_engine::equalizer::Settings::default(),
        sample_rate: rate,
        timing,
        group_id: None,
        handshake_only: false,
        record_mic_path: None,
    };
    options.validate_start()?;
    Ok(options)
}

fn discover(args: &[String]) -> Result<u8> {
    let parsed = Parsed::parse(
        args,
        &["timeout-ms", "service", "host", "port"],
        &["json"],
    )?;
    let timeout = parsed.number::<u64>("timeout-ms")?.unwrap_or(3000);
    let services = match parsed.text("service") {
        Some("raop") => vec![discovery::RAOP_SERVICE],
        Some("airplay") | None => vec![discovery::AIRPLAY_SERVICE],
        Some("all") => vec![discovery::AIRPLAY_SERVICE, discovery::RAOP_SERVICE],
        Some(other) => bail!("usage: --service must be airplay, raop or all, got '{other}'"),
    };
    let target = match (parsed.text("host"), parsed.number::<u16>("port")?) {
        (Some(host), port) => {
            let host: IpAddr = host
                .parse()
                .map_err(|_| anyhow!("usage: --host must be an IP address, got '{host}'"))?;
            std::net::SocketAddr::new(host, port.unwrap_or(5353))
        }
        (None, Some(port)) => std::net::SocketAddr::from((discovery::DEFAULT_MULTICAST.0, port)),
        (None, None) => discovery_target(),
    };
    let found = discovery::discover(&services, Duration::from_millis(timeout), target)?;
    if parsed.flag("json") {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(0);
    }
    if found.is_empty() {
        eprintln!("no AirPlay receivers found on the local network");
        return Ok(0);
    }
    println!("{:<34} {:<24} {:>5}  {:<15} MODEL", "INSTANCE", "HOST", "PORT", "ADDRESS");
    for service in &found {
        let address = service
            .addresses
            .first()
            .map(|a| a.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<34} {:<24} {:>5}  {:<15} {}",
            truncate(&service.instance, 34),
            truncate(&service.host, 24),
            service.port,
            address,
            service.model().unwrap_or("-")
        );
    }
    Ok(0)
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let mut out: String = value.chars().take(width.saturating_sub(1)).collect();
    out.push('~');
    out
}

fn start(args: &[String]) -> Result<u8> {
    let (mut settings, parsed, daemon, quiet) = settings(args)?;
    let layout = layout(&settings, &parsed);
    let timeout = parsed.number::<u64>("timeout-ms")?.unwrap_or(3000);
    resolve_host(&mut settings, timeout)?;
    let options = build_options(&settings)?;
    // A live daemon would lose its own state file to a second start.
    if let Some(state) = State::read(&layout)? {
        if state.pid != std::process::id() && cli::process_alive(state.pid) {
            bail!(
                "session already running (pid {}, {}); stop it first",
                state.pid,
                state.status.status_str()
            );
        }
    }
    let log = settings.log.clone().unwrap_or_else(|| layout.log());
    if daemon {
        let mut child: Vec<std::ffi::OsString> = vec!["start".into(), "--foreground".into()];
        let mut resolved: Vec<(String, String)> = vec![
            ("host".into(), options.peers[0].host.to_string()),
            ("port".into(), options.peers[0].port.to_string()),
            ("source".into(), options.source.clone()),
            ("rate".into(), options.sample_rate.to_string()),
            ("latency-ms".into(), options.latency_ms.to_string()),
            ("gain".into(), options.gain.to_string()),
            ("timing".into(), options.timing.clone()),
            ("codec".into(), settings.codec.clone().unwrap_or_else(|| "auto".into())),
            ("runtime-dir".into(), layout.root().display().to_string()),
        ];
        if !options.wav_path.as_os_str().is_empty() {
            resolved.push(("wav".into(), options.wav_path.display().to_string()));
        }
        // An explicit log path must follow the daemon, not just the parent.
        if let Some(log) = &settings.log {
            resolved.push(("log".into(), log.display().to_string()));
        }
        for (name, value) in resolved {
            child.push(format!("--{name}").into());
            child.push(value.into());
        }
        if quiet {
            child.push("--quiet".into());
        }
        let exe = std::env::current_exe().context("locate airflash-cli")?;
        let pid = cli::spawn_daemon(&exe, &child, &layout, &log)?;
        println!(
            "AirFlash daemon started (pid {pid}) streaming {}:{} from {}",
            options.peers[0].host, options.peers[0].port, options.source
        );
        println!("log: {}", log.display());
        return Ok(0);
    }
    Ok(cli::run_foreground(options, layout, Some(log), quiet)? as u8)
}

fn stop(args: &[String]) -> Result<u8> {
    let parsed = Parsed::parse(args, &["timeout-ms", "runtime-dir"], &["force"])?;
    let layout = Layout::with_root(
        parsed
            .text("runtime-dir")
            .map(PathBuf::from)
            .unwrap_or_else(cli::runtime_dir),
    );
    let timeout = parsed.number::<u64>("timeout-ms")?.unwrap_or(10_000);
    let force = parsed.flag("force");
    let Some(state) = State::read(&layout)? else {
        println!("no AirFlash session is running");
        return Ok(0);
    };
    if !cli::process_alive(state.pid) {
        let _ = std::fs::remove_file(layout.socket());
        let _ = std::fs::remove_file(layout.lock());
        println!("no live AirFlash session (last state: {})", state.status.status_str());
        return Ok(0);
    }
    cli::request_stop(state.pid)?;
    if cli::wait_for_exit(state.pid, Duration::from_millis(timeout), force)? {
        println!("AirFlash session {} stopped", state.pid);
        Ok(0)
    } else {
        bail!("pid {} did not exit within {timeout} ms; retry with --force", state.pid)
    }
}

fn status(args: &[String]) -> Result<u8> {
    let parsed = Parsed::parse(args, &["runtime-dir"], &["json"])?;
    let layout = Layout::with_root(
        parsed
            .text("runtime-dir")
            .map(PathBuf::from)
            .unwrap_or_else(cli::runtime_dir),
    );
    let Some(state) = State::read(&layout)? else {
        if parsed.flag("json") {
            println!("{}", json!({"status":"stopped","pid":null}));
        } else {
            println!("no AirFlash session (no state file)");
        }
        return Ok(0);
    };
    let alive = state.pid != std::process::id() && cli::process_alive(state.pid);
    if parsed.flag("json") {
        let mut value = serde_json::to_value(&state)?;
        value["alive"] = json!(alive);
        value["uptime_ms"] = json!(state.age_ms());
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(0);
    }
    println!(
        "AirFlash session: {}{} (pid {}, up {} ms)",
        state.status.status_str(),
        if alive { "" } else { " (not running)" },
        state.pid,
        state.age_ms()
    );
    println!("  receiver   {}:{}", state.host, state.port);
    println!(
        "  source     {} @ {} Hz",
        state.source, state.sample_rate
    );
    println!("  log        {}", state.log_path.display());
    println!("  control    {}", state.control_socket.display());
    if let Some(error) = &state.error {
        println!("  error      {error}");
    }
    if state.status != Status::Stopped && !alive {
        println!("  note       state is stale; run `airflash-cli stop` to clean up");
    }
    Ok(0)
}

fn logs(args: &[String]) -> Result<u8> {
    let parsed = Parsed::parse(args, &["lines", "runtime-dir"], &[])?;
    let layout = Layout::with_root(
        parsed
            .text("runtime-dir")
            .map(PathBuf::from)
            .unwrap_or_else(cli::runtime_dir),
    );
    let lines = parsed.number::<usize>("lines")?.unwrap_or(40);
    let path = layout.log();
    if !path.exists() {
        println!("no log file at {}", path.display());
        return Ok(0);
    }
    let mut stdout = io::stdout().lock();
    for line in cli::tail(&path, lines)? {
        let _ = writeln!(stdout, "{line}");
    }
    Ok(0)
}

fn control(command: &str, args: &[String]) -> Result<u8> {
    let valued = if command == "volume" {
        vec!["percent", "sequence", "runtime-dir"]
    } else {
        vec!["value", "runtime-dir"]
    };
    let parsed = Parsed::parse(args, &valued, &[])?;
    // Validate the request before touching the daemon so typos never look like
    // a session problem.
    let request = match command {
        "volume" => {
            let percent = parsed
                .number::<u64>("percent")?
                .ok_or_else(|| anyhow!("usage: volume needs --percent 0..100"))?;
            let sequence = parsed
                .number::<u64>("sequence")?
                .ok_or_else(|| anyhow!("usage: volume needs --sequence N"))?;
            ensure!(percent <= 100, "usage: --percent must be 0..100");
            ensure!(sequence > 0, "usage: --sequence must be positive");
            json!({"command":"volume","percent":percent,"sequence":sequence})
        }
        _ => {
            let value = parsed
                .number::<f64>("value")?
                .ok_or_else(|| anyhow!("usage: gain needs --value F"))?;
            ensure!(
                value.is_finite() && (0.0..=1.0).contains(&value),
                "usage: --value must be 0..1"
            );
            json!({"command":"gain","value":value})
        }
    };
    let layout = Layout::with_root(
        parsed
            .text("runtime-dir")
            .map(PathBuf::from)
            .unwrap_or_else(cli::runtime_dir),
    );
    let Some(state) = State::read(&layout)? else {
        bail!("no AirFlash session is running");
    };
    ensure!(
        cli::process_alive(state.pid),
        "AirFlash session {} is not running; start it first",
        state.pid
    );
    let reply = cli::request_control(&layout.socket(), &request)?;
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        println!("{reply}");
        Ok(0)
    } else {
        bail!(
            "receiver rejected the {} command: {}",
            command,
            reply.get("error").and_then(Value::as_str).unwrap_or("unknown error")
        )
    }
}

fn pair(args: &[String]) -> Result<u8> {
    let parsed = Parsed::parse(args, &["host", "port", "pin"], &[])?;
    let host = parsed
        .text("host")
        .ok_or_else(|| anyhow!("usage: pair needs --host IP"))?;
    let host: IpAddr = host
        .parse()
        .map_err(|_| anyhow!("usage: --host must be an IPv4 address, got '{host}'"))?;
    let port = parsed.number::<u16>("port")?.unwrap_or(7000);
    let pin = parsed.text("pin").map(str::to_owned);
    let mut conn = airflash_engine::rtsp::Connection::connect(
        std::net::SocketAddr::new(host, port),
        Cancellation::default(),
    )?;
    let info = conn.request("GET", "/info", &[], &[])?.plist()?;
    let device_id = info
        .as_dictionary()
        .and_then(|d| d.get("deviceID"))
        .and_then(plist::Value::as_string)
        .ok_or_else(|| anyhow!("receiver did not report a device identifier"))?
        .to_owned();
    let credentials = airflash_engine::auth::pair_prompt(&mut conn, || match &pin {
        Some(pin) => Ok(pin.clone()),
        None => {
            eprint!("Enter the AirPlay PIN shown on the receiver: ");
            let _ = io::stderr().flush();
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line)?;
            let pin = line.trim().to_string();
            ensure!(!pin.is_empty(), "no PIN entered");
            Ok(pin)
        }
    })?;
    airflash_engine::credentials::save(
        &airflash_engine::credentials::directory(),
        &device_id,
        &credentials,
    )?;
    println!("paired with {device_id}; credentials stored in {}", airflash_engine::credentials::directory().display());
    Ok(0)
}
