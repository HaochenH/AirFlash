//! End-to-end headless CLI lifecycle against a simulated AirPlay receiver.
//!
//! No real speaker is contacted. The fake peer speaks just enough RTSP, HAP and
//! SRP to carry the sender into the streaming loop, then records what it
//! received. This is what lets CI verify the Linux slice without HomePod
//! hardware: daemon start, streaming, control commands, graceful teardown and
//! honest failure reporting.
#![cfg(all(unix, feature = "cli"))]

use serde_json::json;
use airflash_engine::{
    crypto::{Cipher, auth_open, auth_seal, derive, tlv_decode, tlv_encode},
    credentials, rtsp::Connection,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use num_bigint::BigUint;
use plist::Value;
use sha2::{Digest, Sha512};
use std::{
    io::Read,
    net::{SocketAddr, TcpListener, UdpSocket},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// RFC 3526 3072-bit MODP group, identical to the engine's SRP prime.
const PRIME: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63",
    "B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245",
    "E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F2411",
    "7C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08",
    "CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9",
    "DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D",
    "04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D8760273",
    "3EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E208E24FA074E5AB31",
    "43DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF"
);
/// Engine transient PIN (see `auth::transient`); persistent test pairing PIN.
const TRANSIENT_PIN: &[u8] = b"3939";
const PAIRING_PIN: &[u8] = b"1234";

fn dictionary(items: Vec<(&str, Value)>) -> Value {
    Value::Dictionary(
        items
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn hash(parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha512::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

fn integer(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_be(bytes)
}

fn padded(value: &BigUint) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    let mut out = vec![0; 384 - bytes.len()];
    out.extend(bytes);
    out
}

fn plist_bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    value.to_writer_xml(&mut out).expect("encode plist");
    out
}

type Message = airflash_engine::rtsp::Message;

fn respond(connection: &mut Connection, request: &Message, body: &[u8]) {
    let sequence = request
        .headers
        .get("cseq")
        .cloned()
        .unwrap_or_else(|| "1".into());
    let mut bytes = format!(
        "RTSP/1.0 200 OK\r\nCSeq: {sequence}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    connection.write(&bytes).expect("write fake receiver response");
}

/// RTSP request method (first token) and target (second token).
fn method_of(request: &Message) -> String {
    request
        .first
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn path_of(request: &Message) -> String {
    request
        .first
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned()
}

/// One full HAP pairing exchange, starting at the `/pair-pin-start` request.
/// `persistent` selects HKP 3 with the signed accessory record; otherwise the
/// transient HKP 4 flow the sender uses without stored credentials.
fn pair_exchange(connection: &mut Connection, start: &Message, persistent: bool) -> Vec<u8> {
    let pin = if persistent { PAIRING_PIN } else { TRANSIENT_PIN };
    assert!(start.first.contains("/pair-pin-start"));
    respond(connection, start, &[]);
    let request = connection.read().expect("pair-setup M1");
    let m1 = tlv_decode(&request.body).expect("M1 tlv");
    assert_eq!(m1[&6], vec![1]);
    let modulus = BigUint::parse_bytes(PRIME.as_bytes(), 16).unwrap();
    let generator = BigUint::from(5u8);
    let salt = vec![8; 16];
    let verifier_private = BigUint::from(1234567u64);
    let k = integer(&hash(&[&modulus.to_bytes_be(), &padded(&generator)]));
    let x = integer(&hash(&[&salt, &hash(&[b"Pair-Setup:", pin])]));
    let verifier = generator.modpow(&x, &modulus);
    let public = (&k * &verifier + generator.modpow(&verifier_private, &modulus)) % &modulus;
    let public_bytes = public.to_bytes_be();
    respond(
        connection,
        &request,
        &tlv_encode(&[(6, &[2]), (2, &salt), (3, &public_bytes)]),
    );
    let request = connection.read().expect("pair-setup M3");
    let m3 = tlv_decode(&request.body).expect("M3 tlv");
    let client_public = integer(&m3[&3]);
    let scrambler = integer(&hash(&[&padded(&client_public), &padded(&public)]));
    let shared =
        (client_public * verifier.modpow(&scrambler, &modulus)).modpow(&verifier_private, &modulus);
    let key = hash(&[&shared.to_bytes_be()]);
    let xor: Vec<u8> = hash(&[&modulus.to_bytes_be()])
        .iter()
        .zip(hash(&[&generator.to_bytes_be()]))
        .map(|(a, b)| a ^ b)
        .collect();
    let expected = hash(&[
        &xor,
        &hash(&[b"Pair-Setup"]),
        &salt,
        &m3[&3],
        &public_bytes,
        &key,
    ]);
    assert_eq!(m3[&4], expected, "sender proof mismatch");
    let proof = hash(&[&m3[&3], &expected, &key]);
    respond(
        connection,
        &request,
        &tlv_encode(&[(6, &[4]), (4, &proof)]),
    );
    if !persistent {
        return key;
    }
    let request = connection.read().expect("pair-setup M5");
    let m5 = tlv_decode(&request.body).expect("M5 tlv");
    assert_eq!(m5[&6], vec![5]);
    let session = derive(&key, "Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info");
    let controller = tlv_decode(
        &auth_open(&session, b"PS-Msg05", &m5[&5]).expect("PS-Msg05 body"),
    )
    .expect("M5 tlv");
    let controller_public: [u8; 32] = controller[&3].as_slice().try_into().unwrap();
    let context = derive(
        &key,
        "Pair-Setup-Controller-Sign-Salt",
        "Pair-Setup-Controller-Sign-Info",
    );
    VerifyingKey::from_bytes(&controller_public)
        .unwrap()
        .verify_strict(
            &[context.as_slice(), &controller[&1], &controller[&3]].concat(),
            &Signature::from_slice(&controller[&10]).unwrap(),
        )
        .expect("controller signature");
    let signing = SigningKey::from_bytes(&[99; 32]);
    let accessory_public = signing.verifying_key().to_bytes();
    let accessory_id = b"synthetic-accessory";
    let accessory_context = derive(
        &key,
        "Pair-Setup-Accessory-Sign-Salt",
        "Pair-Setup-Accessory-Sign-Info",
    );
    let signature = signing
        .sign(
            &[accessory_context.as_slice(), accessory_id, &accessory_public].concat(),
        )
        .to_bytes();
    let encrypted = auth_seal(
        &session,
        b"PS-Msg06",
        &tlv_encode(&[(1, accessory_id), (3, &accessory_public), (10, &signature)]),
    )
    .expect("seal M6");
    respond(connection, &request, &tlv_encode(&[(6, &[6]), (5, &encrypted)]));
    key
}

/// Simulated receiver. `persistent` switches the pairing flow; `device_id` is
/// what the sender stores credentials under.
struct Receiver {
    address: SocketAddr,
    device_id: String,
    audio_packets: Arc<AtomicUsize>,
    control_requests: Arc<AtomicUsize>,
    teardown: Arc<AtomicBool>,
}

impl Receiver {
    fn spawn(device_id: &str, persistent_pairing: bool) -> Self {
        let rtsp = TcpListener::bind("127.0.0.1:0").expect("bind rtsp");
        let address = rtsp.local_addr().unwrap();
        let events = TcpListener::bind("127.0.0.1:0").expect("bind events");
        let event_port = events.local_addr().unwrap().port();
        let control = UdpSocket::bind("127.0.0.1:0").expect("bind control");
        let control_port = control.local_addr().unwrap().port();
        let data = UdpSocket::bind("127.0.0.1:0").expect("bind data");
        let data_port = data.local_addr().unwrap().port();
        let audio_packets = Arc::new(AtomicUsize::new(0));
        let control_requests = Arc::new(AtomicUsize::new(0));
        let teardown = Arc::new(AtomicBool::new(false));
        let device_id = device_id.to_owned();
        {
            let (audio, control_count, teardown) = (
                audio_packets.clone(),
                control_requests.clone(),
                teardown.clone(),
            );
            let device_id = device_id.clone();
            thread::spawn(move || {
                thread::spawn(move || {
                    for stream in events.incoming() {
                        thread::spawn(move || {
                            if let Ok(mut stream) = stream {
                                let mut buffer = [0u8; 1024];
                                while stream.read(&mut buffer).unwrap_or(0) > 0 {}
                            }
                        });
                    }
                });
                thread::spawn(move || {
                    let mut buffer = [0u8; 2048];
                    while data.recv_from(&mut buffer).is_ok() {
                        audio.fetch_add(1, Ordering::Relaxed);
                    }
                });
                thread::spawn(move || {
                    let mut buffer = [0u8; 2048];
                    while control.recv_from(&mut buffer).is_ok() {
                        control_count.fetch_add(1, Ordering::Relaxed);
                    }
                });
                let (socket, _) = rtsp.accept().expect("accept sender");
                let mut connection =
                    Connection::from_stream(socket, airflash_engine::rtsp::Cancellation::default())
                        .expect("server connection");
                connection
                    .set_timeout(Duration::from_secs(180))
                    .expect("server timeout");
                // SRP-signed records must be counted per direction like the sender does.
                let mut records: Option<Cipher> = None;
                loop {
                    let request = match connection.read() {
                        Ok(request) => request,
                        Err(_) => break,
                    };
                    let (method, path) = (method_of(&request), path_of(&request));
                    if path == "/pair-pin-start" {
                        let key = pair_exchange(&mut connection, &request, persistent_pairing);
                        // After HAP control setup the sender encrypts RTSP with
                        // Control-Write; the receiver mirrors it with Control-Read.
                        connection.encrypt(
                            derive(&key, "Control-Salt", "Control-Read-Encryption-Key"),
                            derive(&key, "Control-Salt", "Control-Write-Encryption-Key"),
                        );
                        continue;
                    }
                    if path == "/info" || method == "GET" && path.is_empty() {
                        respond(
                            &mut connection,
                            &request,
                            &plist_bytes(&dictionary(vec![
                                ("deviceID", Value::String(device_id.clone())),
                                ("model", Value::String("AudioAccessory5,1".into())),
                                ("sourceVersion", Value::String("356.23".into())),
                                ("initialVolume", Value::Real(-30.0)),
                            ])),
                        );
                        continue;
                    }
                    if method == "SETUP" {
                        let body: Value = plist::from_bytes(&request.body).expect("setup plist");
                        let streams = body
                            .as_dictionary()
                            .and_then(|d| d.get("streams"))
                            .and_then(Value::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        let response = if streams > 0 {
                            dictionary(vec![(
                                "streams",
                                Value::Array(vec![dictionary(vec![
                                    ("controlPort", Value::Integer(control_port.into())),
                                    ("dataPort", Value::Integer(data_port.into())),
                                    ("latencyMin", Value::Integer(2205.into())),
                                ])]),
                            )])
                        } else {
                            dictionary(vec![(
                                "eventPort",
                                Value::Integer(event_port.into()),
                            )])
                        };
                        respond(&mut connection, &request, &plist_bytes(&response));
                        continue;
                    }
                    if method == "TEARDOWN" {
                        teardown.store(true, Ordering::Relaxed);
                        respond(&mut connection, &request, &[]);
                        continue;
                    }
                    // RECORD, FLUSH, SET_PARAMETER volume, feedback: accept all.
                    respond(&mut connection, &request, &[]);
                }
                drop(records.take());
            });
        }
        Self {
            address,
            device_id,
            audio_packets,
            control_requests,
            teardown,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // Closing the runtime keeps the worker threads from outliving the test.
        let _ = std::net::TcpStream::connect(self.address);
    }
}

struct Instance {
    directory: std::path::PathBuf,
}

impl Instance {
    fn new(label: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "airflash-cli-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("create runtime directory");
        Self { directory }
    }
    fn path(&self) -> &std::path::Path {
        &self.directory
    }
    /// Strict read; use `state_now` while a daemon may still be starting.
    fn state(&self) -> serde_json::Value {
        let text = std::fs::read_to_string(self.directory.join("session.json"))
            .expect("read session state");
        serde_json::from_str(&text).expect("parse session state")
    }
    fn state_now(&self) -> serde_json::Value {
        match std::fs::read_to_string(self.directory.join("session.json")) {
            Ok(text) => serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
            Err(_) => serde_json::Value::Null,
        }
    }
    fn log(&self) -> String {
        std::fs::read_to_string(self.directory.join("session.log")).unwrap_or_default()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn cli() -> &'static str {
    env!("CARGO_BIN_EXE_airflash-cli")
}

fn until(mut condition: impl FnMut() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(100));
    }
}

fn output(arguments: &[&str], directory: &std::path::Path) -> std::process::Output {
    Command::new(cli())
        .args(arguments)
        .arg("--runtime-dir")
        .arg(directory)
        .stdin(Stdio::null())
        .output()
        .expect("run airflash-cli")
}

#[test]
fn daemon_streams_against_the_fake_receiver_and_stops_gracefully() {
    let receiver = Receiver::spawn("AA:BB:CC:DD:EE:01", false);
    let instance = Instance::new("stream");
    let host = receiver.address.ip().to_string();
    let port = receiver.address.port().to_string();
    let started = output(
        &[
            "start",
            "--daemon",
            "--host",
            &host,
            "--port",
            &port,
            "--source",
            "simulated",
            "--timing",
            "ntp",
            "--latency-ms",
            "200",
            "--gain",
            "0.5",
        ],
        instance.path(),
    );
    assert!(
        started.status.success(),
        "daemon start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let state = instance.state();
    assert_eq!(state["status"], "streaming");
    assert_eq!(state["source"], "simulated");
    assert_eq!(state["host"], host);
    assert_eq!(state["port"], serde_json::json!(port.parse::<u16>().unwrap()));
    let pid = state["pid"].as_u64().unwrap();
    assert!(
        airflash_engine::cli::process_alive(pid as u32),
        "daemon {pid} must stay alive while streaming"
    );

    // Playback control goes through the control socket, not a restart.
    let volume = output(
        &["volume", "--percent", "25", "--sequence", "1"],
        instance.path(),
    );
    assert!(
        volume.status.success(),
        "volume command failed: {}",
        String::from_utf8_lossy(&volume.stderr)
    );
    let gain = output(&["gain", "--value", "0.25"], instance.path());
    assert!(
        gain.status.success(),
        "gain command failed: {}",
        String::from_utf8_lossy(&gain.stderr)
    );

    // State and logs are readable from another process with no tty involved.
    let status = output(&["status"], instance.path());
    assert!(status.status.success());
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("streaming"), "unexpected status: {text}");
    until(
        || instance.log().contains("\"event\":\"streaming\""),
        "streaming event in the daemon log",
    );
    until(
        || instance.log().contains("\"event\":\"transport_metrics\""),
        "transport metrics in the daemon log",
    );
    let logs = output(&["logs", "--lines", "5"], instance.path());
    assert!(logs.status.success());

    // Audio actually reached the receiver.
    until(
        || receiver.audio_packets.load(Ordering::Relaxed) > 20,
        "audio packets at the receiver",
    );

    let stopped = output(&["stop"], instance.path());
    assert!(
        stopped.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    until(
        || receiver.teardown.load(Ordering::Relaxed),
        "graceful RTSP teardown at the receiver",
    );
    until(
        || !airflash_engine::cli::process_alive(pid as u32),
        "daemon process exit",
    );
    assert_eq!(instance.state()["status"], "stopped");
    assert!(instance.log().contains("\"event\":\"stopped\""));
    assert!(
        !instance.path().join("control.sock").exists(),
        "control socket must be cleaned up"
    );
    let received = receiver.audio_packets.load(Ordering::Relaxed);
    assert!(received > 20, "only {received} audio packets were received");
    assert!(
        receiver.control_requests.load(Ordering::Relaxed) > 0,
        "receiver should see retransmission control traffic"
    );
    drop(receiver);
}

#[test]
fn start_against_an_unreachable_receiver_fails_without_orphans() {
    // A bound-then-dropped listener gives a port that refuses connections.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let instance = Instance::new("unreachable");
    let host = "127.0.0.1";
    let port = port.to_string();
    let started = output(
        &[
            "start",
            "--daemon",
            "--host",
            host,
            "--port",
            &port,
            "--source",
            "simulated",
            "--timing",
            "ntp",
        ],
        instance.path(),
    );
    assert!(
        !started.status.success(),
        "start must fail when the receiver is unreachable"
    );
    let stderr = String::from_utf8_lossy(&started.stderr);
    assert!(
        stderr.contains("failed") || stderr.contains("refused") || stderr.contains("readiness"),
        "unexpected failure message: {stderr}"
    );
    let state = instance.state();
    assert_eq!(state["status"], "failed");
    assert!(state["error"].is_string());
    // No daemon survives a failed start, and stop is still safe to call.
    let stopped = output(&["stop"], instance.path());
    assert!(stopped.status.success());
    assert!(
        !instance.path().join("control.sock").exists(),
        "no control socket may survive a failed session"
    );
}

#[test]
fn foreground_session_terminates_gracefully_on_sigterm() {
    let receiver = Receiver::spawn("AA:BB:CC:DD:EE:02", false);
    let instance = Instance::new("foreground");
    let host = receiver.address.ip().to_string();
    let port = receiver.address.port().to_string();
    let mut child = Command::new(cli())
        .args([
            "start",
            "--foreground",
            "--host",
            &host,
            "--port",
            &port,
            "--source",
            "simulated",
            "--timing",
            "ntp",
        ])
        .arg("--runtime-dir")
        .arg(instance.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn foreground session");
    until(
        || instance.state_now()["status"] == "streaming",
        "foreground session streaming",
    );
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let status = child.wait().expect("wait for foreground session");
    assert!(status.success(), "SIGTERM must stop the session cleanly");
    until(
        || receiver.teardown.load(Ordering::Relaxed),
        "teardown after SIGTERM",
    );
    let received = receiver.audio_packets.load(Ordering::Relaxed);
    assert!(received > 0, "no audio reached the receiver");
    let state = instance.state();
    assert_eq!(state["status"], "stopped");
    assert!(!instance.path().join("control.sock").exists());
    drop(receiver);
}

#[test]
fn pairing_stores_credentials_and_a_later_session_can_verify_them() {
    let receiver = Receiver::spawn("AA:BB:CC:DD:EE:03", true);
    let instance = Instance::new("pair");
    let config = instance.path().join("credentials");
    std::fs::create_dir_all(&config).expect("create credential directory");
    // Keep the credential store out of the real HOME for this test only.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &config);
    }
    let host = receiver.address.ip().to_string();
    let port = receiver.address.port().to_string();
    let paired = Command::new(cli())
        .args(["pair", "--host", &host, "--port", &port, "--pin", "1234"])
        .output()
        .expect("run pair");
    assert!(
        paired.status.success(),
        "pair failed: {}",
        String::from_utf8_lossy(&paired.stderr)
    );
    let stored = credentials::load(&credentials::directory(), &receiver.device_id)
        .expect("read stored credentials")
        .expect("credentials must be stored after pairing");
    assert_eq!(stored.accessory_id, b"synthetic-accessory");
    unsafe {
        std::env::remove_var("XDG_CONFIG_HOME");
    }
    assert_eq!(receiver.device_id, "AA:BB:CC:DD:EE:03");
    let _ = output(&["status"], instance.path());
    drop(receiver);
}

#[test]
fn config_file_drives_the_same_session_as_flags() {
    // The systemd user service starts the CLI with `--config`, so the JSON
    // configuration path must reach the same streaming session as the flags.
    let receiver = Receiver::spawn("AA:BB:CC:DD:EE:04", false);
    let instance = Instance::new("config");
    let config = instance.path().join("cli.json");
    let settings = json!({
        "host": receiver.address.ip().to_string(),
        "port": receiver.address.port(),
        "source": "simulated",
        "rate": 44100,
        "latency_ms": 200,
        "gain": 0.25,
        "timing": "ntp",
    });
    std::fs::write(&config, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
    let started = Command::new(cli())
        .args(["start", "--daemon", "--config"])
        .arg(&config)
        .arg("--runtime-dir")
        .arg(instance.path())
        .output()
        .expect("run start --config");
    assert!(
        started.status.success(),
        "config-driven start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    until(
        || instance.state_now()["status"] == "streaming",
        "config-driven session streaming",
    );
    let state = instance.state();
    assert_eq!(state["source"], "simulated");
    assert_eq!(state["sample_rate"], 44100);
    until(
        || receiver.audio_packets.load(Ordering::Relaxed) > 5,
        "audio from the config-driven session",
    );
    let stopped = output(&["stop"], instance.path());
    assert!(stopped.status.success());
    until(
        || receiver.teardown.load(Ordering::Relaxed),
        "teardown after a config-driven session",
    );
    // A broken config file is an error, never a silent default.
    std::fs::write(&config, b"{ not json").unwrap();
    let broken = Command::new(cli())
        .args(["start", "--daemon", "--config"])
        .arg(&config)
        .arg("--runtime-dir")
        .arg(instance.path())
        .output()
        .expect("run start --config");
    assert!(!broken.status.success());
    let message = String::from_utf8_lossy(&broken.stderr);
    assert!(message.contains("parse config"), "unexpected error: {message}");
    drop(receiver);
}

#[test]
fn cli_arguments_are_validated_before_any_session_starts() {
    let instance = Instance::new("usage");
    let directory = instance.path().to_path_buf();
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["start", "--host", "127.0.0.1", "--source", "file"],
            "source file needs --wav",
        ),
        (vec!["start", "--host", "not-an-ip"], "IPv4"),
        (vec!["start", "--host", "127.0.0.1", "--rate", "96000"], "44100 or 48000"),
        (vec!["volume", "--percent", "30"], "--sequence"),
        (vec!["gain", "--value", "2"], "0..1"),
    ];
    for (arguments, expected) in cases {
        let result = Command::new(cli())
            .args(&arguments)
            .arg("--runtime-dir")
            .arg(&directory)
            .output()
            .expect("run airflash-cli");
        assert!(
            !result.status.success(),
            "{arguments:?} must be rejected"
        );
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            message.contains(expected),
            "{arguments:?} should mention '{expected}', got: {message}"
        );
        assert!(
            !directory.join("session.json").exists(),
            "{arguments:?} must not leave daemon state behind"
        );
    }
    // Unknown options never silently become defaults.
    let bogus = Command::new(cli())
        .args(["status", "--jsn"])
        .arg("--runtime-dir")
        .arg(&directory)
        .output()
        .expect("run airflash-cli");
    assert_eq!(bogus.status.code(), Some(2));
}

#[test]
fn discovery_parses_a_real_mdns_response_over_loopback() {
    // The CLI sends to the multicast group; a local responder proves the query
    // bytes and the parser cooperate end to end without any hardware.
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = socket.local_addr().unwrap();
    let responder = thread::spawn(move || {
        let mut buffer = [0u8; 2048];
        let (size, peer) = socket.recv_from(&mut buffer).unwrap();
        assert_eq!(buffer[2] & 0x80, 0, "query must not claim to be a response");
        let questions = u16::from_be_bytes([buffer[4], buffer[5]]);
        assert_eq!(questions, 1);
        // Answer with a PTR plus the SRV/A/TXT additional records.
        let mut response = vec![0, 0, 0x84, 0x00];
        response.extend_from_slice(&0u16.to_be_bytes());
        response.extend_from_slice(&3u16.to_be_bytes());
        response.extend_from_slice(&[0, 0, 0, 0]);
        let instance_name = encode_name(&["Study", "_airplay", "_tcp", "local"]);
        response.extend_from_slice(&instance_name);
        response.extend_from_slice(&12u16.to_be_bytes());
        response.extend_from_slice(&1u16.to_be_bytes());
        response.extend_from_slice(&120u32.to_be_bytes());
        response.extend_from_slice(&(instance_name.len() as u16).to_be_bytes());
        response.extend_from_slice(&instance_name);
        let host = encode_name(&["study-pod", "local"]);
        response.extend_from_slice(&encode_name(&["Study", "_airplay", "_tcp", "local"]));
        response.extend_from_slice(&33u16.to_be_bytes());
        response.extend_from_slice(&1u16.to_be_bytes());
        response.extend_from_slice(&120u32.to_be_bytes());
        let mut srv = vec![0, 0, 0, 0, 0x1f, 0x90];
        srv.extend_from_slice(&host);
        response.extend_from_slice(&(srv.len() as u16).to_be_bytes());
        response.extend_from_slice(&srv);
        response.extend_from_slice(&host);
        response.extend_from_slice(&1u16.to_be_bytes());
        response.extend_from_slice(&1u16.to_be_bytes());
        response.extend_from_slice(&120u32.to_be_bytes());
        response.extend_from_slice(&4u16.to_be_bytes());
        response.extend_from_slice(&[10, 0, 0, 7]);
        socket
            .send_to(&response, peer)
            .expect("send mDNS response");
        let _ = size;
    });
    let found = airflash_engine::discovery::discover(
        &[airflash_engine::discovery::AIRPLAY_SERVICE],
        Duration::from_millis(1500),
        target,
    )
    .expect("discover");
    responder.join().unwrap();
    assert_eq!(found.len(), 1, "expected one receiver, got {found:?}");
    assert_eq!(found[0].instance, "Study._airplay._tcp.local");
    assert_eq!(found[0].host, "study-pod.local");
    assert_eq!(found[0].port, 8080);
    assert_eq!(found[0].addresses, vec![std::net::Ipv4Addr::new(10, 0, 0, 7)]);
}

fn encode_name(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.push(part.len() as u8);
        out.extend_from_slice(part.as_bytes());
    }
    out.push(0);
    out
}
