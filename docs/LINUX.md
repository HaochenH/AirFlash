# AirFlash on Linux (headless slice)

This document covers the Linux side of AirFlash: the headless `airflash-cli`
binary, background operation, `systemd --user` operation, the AppImage, and the
audio boundaries that are **not** implemented yet.

AirFlash on Linux is a preview. It reuses the same Rust sender, HAP, PTP and
encrypted RTP path as the Windows application, driven by a command line instead
of a graphical UI. It is not a port of the WPF interface.

## Status

| Capability | Windows | Linux (this slice) |
| --- | --- | --- |
| AirPlay 2 sender (PCM, encrypted RTP) | yes | yes |
| HAP pairing, transient + persistent | yes | yes |
| Stereo pairs | yes | yes (protocol level, untested on hardware) |
| Automatic discovery (mDNS) | yes (DNS-SD API) | yes (built-in mDNS querier) |
| System audio capture | yes (WASAPI loopback) | **no — see [Audio](#audio-input-and-its-limits)** |
| GUI / tray / settings | yes | no |
| HomePod qualification on hardware | yes | not performed |

## Dependencies

Runtime:

- glibc (any current distribution), no root privileges
- `libc` only, plus the C runtime; see `ldd usr/bin/airflash-cli`

Build:

- Rust stable 1.85+ (`rustup`), `cargo`
- The `cli` feature enables the headless binary:
  `cargo build --release --features cli --manifest-path native/airflash-engine/Cargo.toml`
- x86_64 cross builds need `gcc-x86-64-linux-gnu` on other hosts
- AppImage packaging needs `curl` (to fetch `appimagetool`) and `mksquashfs`
  (squashfs-tools)

Install the CLI:

```bash
cargo build --release --features cli --manifest-path native/airflash-engine/Cargo.toml
sudo install -m0755 native/airflash-engine/target/release/airflash-cli /usr/local/bin/airflash-cli
```

## Quick start

```bash
# 1. Find receivers on the local network.
airflash-cli discover

# 2. Pair once (PIN shown on the HomePod; prompts on stdin).
airflash-cli pair --host 192.168.1.42

# 3. Stream in the foreground, one JSON event per line.
airflash-cli start --host 192.168.1.42 --source simulated

# 4. Or run it detached in the background.
airflash-cli start --host 192.168.1.42 --source simulated --daemon
airflash-cli status
airflash-cli logs --lines 20
airflash-cli volume --percent 30 --sequence 1
airflash-cli stop
```

`airflash-cli help` prints the full command list. Every command works without a
graphical desktop and without a tty: output is either human readable text or
JSON (`--json`), and errors go to stderr with a non-zero exit code.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success (including "nothing to stop") |
| 1 | runtime failure, message on stderr |
| 2 | usage error (unknown option, missing value) |

## Commands

| Command | Purpose |
| --- | --- |
| `discover [--timeout-ms N] [--json] [--service airplay\|raop\|all]` | mDNS browse for `_airplay._tcp` / `_raop._tcp` receivers |
| `start --host IP [--port N] [--source file\|simulated\|loopback] [--wav PATH] [--rate N] [--latency-ms N] [--gain F] [--timing ptp\|ntp] [--config PATH] [--daemon\|--foreground] [--log PATH] [--runtime-dir PATH] [--quiet]` | open a session and stream |
| `start --device NAME` | resolve the receiver through mDNS first |
| `stop [--timeout-ms N] [--force]` | graceful stop (SIGTERM, then SIGKILL with `--force`) |
| `status [--json]` | session state, receiver, uptime, last error |
| `logs [--lines N]` | tail the session log (JSONL engine events) |
| `volume --percent N --sequence N` | receiver volume through the control channel |
| `gain --value F` | sender-side master gain, 0..1 |
| `pair --host IP [--port N] [--pin N]` | HAP pairing; PIN from stdin or `--pin` |
| `version` | binary version |

`--config PATH` reads a JSON file with the same keys as the flags (snake_case:
`host`, `port`, `source`, `wav`, `rate`, `latency_ms`, `gain`, `timing`, `log`,
`runtime_dir`). Explicit flags override the file. The systemd units use it.

### Timing

`--timing ptp` is the AirPlay default and matches the Windows application.
PTP uses UDP ports 319/320, which are **privileged**: an unprivileged process
can only bind them when `net.ipv4.ip_unprivileged_port_start` is 319 or lower
(the binary has no capabilities granted). Use `--timing ntp` on hosts where you
cannot change that sysctl, and note that NTP timing is not the AirPlay-preferred
profile. The example systemd configuration uses `ntp` for this reason.

## Background operation

`airflash-cli start --daemon` re-executes itself detached:

- stdin is `/dev/null`, stdout/stderr are appended to the session log
- the parent waits until the child reports streaming (or fails) and then exits
- state, log and control socket live in one runtime directory:

| Variable | Value |
| --- | --- |
| `AIRFLASH_RUNTIME_DIR` | explicit override, always wins |
| `$XDG_RUNTIME_DIR/airflash` | default when a session runtime dir exists |
| `~/.local/state/airflash` | fallback without XDG_RUNTIME_DIR |

The runtime directory is created with mode `0700` and contains:

| File | Purpose |
| --- | --- |
| `session.json` | state: pid, status, receiver, source, error |
| `session.log` | JSONL engine events and daemon lifecycle lines |
| `control.sock` | Unix socket for `volume` / `gain` / `status` |
| `daemon.lock` | single-instance lock (stale locks are reclaimed) |

The directory is `0700`, so the control socket is reachable only by the owning
user; no network port is opened for control. Engine events never contain keys,
PINs or credential material.

Only one daemon may use a runtime directory at a time. A second `start` fails
with `another AirFlash daemon is running`; a lock left by a `SIGKILL`ed process
is reclaimed automatically once its pid is gone.

`stop` sends SIGTERM. The daemon turns that into a graceful cancel: RTSP
`TEARDOWN` is sent to every member, streams stop, final metrics are written, and
the exit code is 0. `--force` escalates to SIGKILL after the timeout.

`status` reports the on-disk state plus whether the recorded pid is alive, so a
stale state file is visible instead of silently reported as streaming.

## systemd --user

Two units ship in `packaging/linux/systemd/user/`:

| Unit | Use |
| --- | --- |
| `airflash-cli.service` | one receiver, configured in `~/.config/airflash/cli.json` |
| `airflash-cli@.service` | one instance per receiver: `airflash-cli@living-room.service` |

Install (no root needed):

```bash
mkdir -p ~/.config/systemd/user ~/.config/airflash
cp packaging/linux/systemd/user/airflash-cli.service ~/.config/systemd/user/
cp packaging/linux/config/cli.json.example ~/.config/airflash/cli.json
$EDITOR ~/.config/airflash/cli.json
systemctl --user daemon-reload
systemctl --user enable --now airflash-cli.service
journalctl --user -u airflash-cli.service -f
```

For the template unit, name the profile with letters, digits, `.`, `-` or `_`
and create `~/.config/airflash/cli-<name>.json`:

```bash
systemctl --user enable --now airflash-cli@living-room.service
```

What the units do:

- `Type=simple`, `ExecStart=... start --foreground --quiet --config ...`
- `Environment=AIRFLASH_RUNTIME_DIR=%t/airflash` (`%i` for the template), so
  state never lands in `/tmp`
- `EnvironmentFile=-%h/.config/airflash/cli.env` (the `-` makes it optional);
  use it for extra variables only
- `Restart=on-failure`, `RestartSec=5`, `StartLimitBurst=5` in 300s: a
  misconfiguration retries a few times, then stays failed and visible
- `KillSignal=SIGTERM`, `TimeoutStopSec=20`: graceful teardown is expected; the
  daemon exits on its own, so no `ExecStop` is needed
- `StandardOutput=journal`, `SyslogIdentifier=airflash-cli`
- `WantedBy=default.target`

Because this is a *user* unit, it inherits the login session's
`DBUS_SESSION_BUS_ADDRESS` and `XDG_RUNTIME_DIR`, which is what a PipeWire or
PulseAudio client needs. A unit started outside a session (cron, a bare
container) does not have those; set them in the env file if you do that.

To survive logouts, enable linger for your user (an administrator action):

```bash
sudo loginctl enable-linger "$USER"
```

## AppImage

```bash
packaging/linux/appimage/build-appimage.sh
```

- Produces `dist/AppImage/AirFlash-<version>-x86_64.AppImage` and `SHA256SUMS.txt`
- `VERSION`, `TARGET`, `ARCH`, `OUTPUT_DIR`, `BUILD_DIR` and
  `AIRFLASH_APPIMAGETOOL` (path to an existing tool) override the defaults
- The script is repeatable: it rebuilds the AppDir from scratch each run and
  verifies AppRun, the desktop entry, the icon and the version before packaging
- `appimagetool` is downloaded and extracted (no FUSE needed) unless
  `AIRFLASH_APPIMAGETOOL` points at an installed copy; `SKIP_APPIMAGETOOL_DOWNLOAD=1`
  makes a missing tool an error instead of a download
- Cross builds need a cross linker, e.g. `gcc-x86-64-linux-gnu` on an aarch64 host
- Windows release artifacts (`dist/AirFlash.exe`, `dist/AirFlash-*.msi`) are not
  touched by any part of this packaging

AppDir layout:

```
AirFlash.AppDir/
├── AppRun                     # executable wrapper, fills runtime locations
├── .DirIcon / airflash-cli.png# icon used by appimagetool and desktops
├── airflash-cli.desktop      # desktop entry (X-AppImage-Version substituted)
├── VERSION                   # plain version text
└── usr/
    ├── bin/airflash-cli      # the CLI
    ├── bin/airflash-engine   # the JSONL engine, for the Python probe harness
    ├── share/applications/airflash-cli.desktop
    ├── share/icons/hicolor/512x512/apps/airflash-cli.png
    ├── share/doc/airflash-cli/README
    └── share/licenses/airflash-cli/{LICENSE-GPLv3,LICENSE-COMMERCIAL.md}
```

Running the AppImage without installing: `./AirFlash-<version>-x86_64.AppImage discover`.
`AppRun` exports `AIRFLASH_RUNTIME_DIR` when neither it nor `XDG_RUNTIME_DIR` is
set, so the CLI works from a file manager as well as from a terminal.

## Audio input and its limits

This is the honest boundary of the current slice:

- **PipeWire and PulseAudio capture is not implemented.** There is no Linux
  system-audio capture yet; `--source loopback` on Linux fails with an explicit
  error instead of streaming silence or pretending to capture.
- Two testable inputs exist so the transport path can be exercised without a
  desktop mixer:
  - `--source simulated` — a deterministic stereo signal (220 Hz left / 330 Hz
    right, one second of tone then one second of silence). Reproducible output,
    which is what the automated tests assert on.
  - `--source file --wav PATH` — loops a WAV file (16/24/32-bit integer or
    32-bit float PCM, mono or multichannel, any rate, resampled to the streaming
    rate, up to ten minutes).
- The WAV/loopback capture queue, resampling, equalizer, master gain, underrun
  silence padding and scheduler recovery are the same code the Windows loopback
  uses, so behaviour and metrics are comparable across platforms.
- `airflash-cli status`, `logs` and the `capture_metrics` events report
  `input_rate`, `underrun_packets`, `dropped_frames` and queue-age percentiles
  for the selected input, so file or simulated captures are never presented as
  desktop audio.

Consequences: streaming from real desktop audio on Linux requires a future
PipeWire/PulseAudio capture implementation. Until then, the CLI plus AppImage are
useful for verification, pairing, discovery and CI, and for streaming a chosen
file or signal to a HomePod.

## Known limitations

- No graphical UI, tray, autostart or settings window on Linux.
- No system-audio capture (see above).
- `discover` implements a minimal mDNS querier: PTR/SRV/A/TXT parsing with
  compression pointers, no continuous browsing, no link-local IPv6 answers and
  no known-answer suppression. It is a CLI query, not a general DNS-SD stack.
- PTP timing needs privileged ports 319/320 unless
  `net.ipv4.ip_unprivileged_port_start` is lowered; no capabilities are
  requested or set.
- Equalizer control is available in the engine but is not exposed on the CLI.
- HomePod hardware qualification has not been performed on Linux. The protocol
  path is exercised against a simulated receiver in the test suite only.
- No code signing or update channel for the AppImage; verify `SHA256SUMS.txt`
  out of band.
- Only x86_64 is packaged; the build script accepts other targets, but they are
  untested.

## Tests

```bash
cargo test --features cli --manifest-path native/airflash-engine/Cargo.toml
cargo clippy --all-targets --features cli --manifest-path native/airflash-engine/Cargo.toml -- -D warnings
AIRFLASH_BUILD_APPIMAGE=1 python -m pytest tests/test_linux_packaging.py
```

The Rust suite includes the headless CLI lifecycle tests (daemon start, fake
receiver streaming, control commands, graceful stop, failure without orphans,
SIGTERM handling, config files, argument validation) and the simulated audio
input tests.
