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

## Graphical frontend (Avalonia)

The Windows panel is WPF. On Linux the same interface ships as an **Avalonia UI**
application that reuses the shared Core layer (session controller, settings,
receiver catalog, equalizer, localization) and only re-implements the
toolkit-specific parts (dispatcher, timers, discovery backend, paths).

| Windows project | Linux counterpart |
| --- | --- |
| `desktop/AirFlash.App` (WPF) | `desktop/AirFlash.UI` (Avalonia) |
| `WindowsDiscovery` (DNS-SD API) | `LinuxDiscovery` — runs `airflash-cli discover --json` and feeds `ReceiverAggregator` |
| `AudioService` (NAudio endpoints) | `LinuxAudioService` — the sender owns capture; no mixer session |
| `Autostart` (Run key) | `LinuxAutostart` — `~/.config/autostart/airflash-ui.desktop` |
| `AppPaths` (AppData) | `AppPaths` — XDG config/state directories |

The view models are ports of the Windows ones; both take the same shared
`AirFlash.Core` types, so settings, pairing, reconnect logic and the diagnostics
pages behave identically.

Build and run:

```bash
dotnet build desktop/AirFlash.UI/AirFlash.UI.csproj
dotnet test  desktop/AirFlash.UI.Tests/AirFlash.UI.Tests.csproj
AIRFLASH_ENGINE=$PWD/native/airflash-engine/target/release/airflash-engine \
AIRFLASH_CLI=$PWD/native/airflash-engine/target/release/airflash-cli \
dotnet run --project desktop/AirFlash.UI/AirFlash.UI.csproj
```

Environment overrides: `AIRFLASH_ENGINE`, `AIRFLASH_CLI`, `AIRFLASH_DATA_DIR`
(settings), `AIRFLASH_RUNTIME_DIR` (state, logs, single-instance lock).

First run selects the **simulated test signal**, because system capture is
Windows-only today. Switch the input in **Settings → Audio source**.

## Dependencies

Runtime:

- glibc (any current distribution), no root privileges
- `libc` only, plus the C runtime; see `ldd usr/bin/airflash-cli`

Build:

- Rust stable 1.85+ (`rustup`), `cargo`
- .NET SDK 10.0+ for the graphical frontend (`dotnet`)
- Avalonia 11.3 packages are restored from NuGet; the script pins
  `Tmds.DBus.Protocol` 0.21.3 because the transitive 0.21.2 has a published
  advisory (GHSA-xrw6-gwf8-vvr9)
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
| `start --host IP [--port N] [--source file\|simulated\|loopback] [--wav PATH] [--rate N] [--latency-ms N] [--gain F] [--timing ptp\|ntp\|auto] [--codec alac\|pcm\|auto] [--monitor NAME] [--config PATH] [--daemon\|--foreground] [--log PATH] [--runtime-dir PATH] [--quiet]` | open a session and stream |
| `start --device NAME` | resolve the receiver through mDNS first |
| `stop [--timeout-ms N] [--force]` | graceful stop (SIGTERM, then SIGKILL with `--force`) |
| `status [--json]` | session state, receiver, uptime, last error |
| `logs [--lines N]` | tail the session log (JSONL engine events) |
| `volume --percent N --sequence N` | receiver volume through the control channel |
| `gain --value F` | sender-side master gain, 0..1 |
| `pair --host IP [--port N] [--pin N]` | HAP pairing; PIN from stdin or `--pin` |
| `version` | binary version |

`--config PATH` reads a JSON file with the same keys as the flags (snake_case:
`host`, `port`, `source`, `wav`, `rate`, `latency_ms`, `gain`, `timing`, `codec`,
`monitor`, `log`, `runtime_dir`). Explicit flags override the file. The systemd
units use it. `--device NAME` resolves the receiver through mDNS first and picks
up its advertised codecs; `--monitor NAME` selects a PipeWire sink (node name,
id or description) instead of the default sink monitor.

### Timing

`--timing auto` (the default) uses PTP when UDP ports 319/320 can be bound and
falls back to NTP otherwise, with a `timing_fallback` warning event. PTP is the
AirPlay default, matches the Windows application, and is **required by AirPlay 2
receivers such as HomePods**: an NTP-timed session connects and lights the
receiver, but the audio SETUP is rejected with `400 Bad Request`.

Ports 319/320 are **privileged**. Grant them with one of:

```bash
sudo setcap cap_net_bind_service=+ep /usr/bin/airflash-cli
sudo sysctl -w net.ipv4.ip_unprivileged_port_start=319
```

or, for the systemd unit, `AmbientCapabilities=CAP_NET_BIND_SERVICE` (see the
commented lines in `packaging/linux/systemd/user/airflash-cli.service`).
`--timing ntp` remains available for receivers that accept it.

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

Two variants come from one script:

```bash
packaging/linux/appimage/build-appimage.sh                  # headless only
VARIANT=full packaging/linux/appimage/build-appimage.sh     # adds the panel
```

| Variant | Artifact | Contents |
| --- | --- | --- |
| `backend` | `AirFlash-<version>-x86_64.AppImage` | `airflash-cli`, `airflash-engine` |
| `full` | `AirFlash-<version>-x86_64-full.AppImage` | the above plus the Avalonia panel and a `Terminal=false` desktop entry |

`ARCH`/`TARGET` select the architecture, so the same script produces the aarch64
pair on an aarch64 host:

```bash
VARIANT=full TARGET=aarch64-unknown-linux-gnu ARCH=aarch64 DOTNET_RUNTIME=linux-arm64 \
  packaging/linux/appimage/build-appimage.sh
```

A cross build skips `appimagetool` — it is itself an AppImage and cannot run on a
foreign architecture — and assembles the image from the official type2 runtime plus
`squashfs-tools`, so x86_64 images can be produced from an aarch64 machine and the
other way round.

In the full image, running the AppImage with no arguments opens the panel;
`AirFlash.AppImage discover` (or any CLI command) still drives the headless
binary, and `AirFlash.AppImage gui` forces the panel.

- Produces `dist/AppImage/…AppImage` and `SHA256SUMS.txt`
- `VERSION`, `TARGET`, `ARCH`, `OUTPUT_DIR`, `BUILD_DIR` and
  `AIRFLASH_APPIMAGETOOL` (path to an existing tool) override the defaults
- The script is repeatable: it rebuilds the AppDir from scratch each run and
  verifies AppRun, the desktop entry, the icon and the version before packaging
- `appimagetool` is downloaded and extracted (no FUSE needed) unless
  `AIRFLASH_APPIMAGETOOL` points at an installed copy; `SKIP_APPIMAGETOOL_DOWNLOAD=1`
  makes a missing tool an error instead of a download
- Cross builds need a cross linker, e.g. `gcc-x86-64-linux-gnu` on an aarch64 host
- `VARIANT=full` needs the .NET SDK and publishes a self-contained runtime
  (`DOTNET_RUNTIME` selects it; `linux-arm64` on an aarch64 host)
- Windows release artifacts (`dist/AirFlash.exe`, `dist/AirFlash-*.msi`) are not
  touched by any part of this packaging

AppDir layout:

```
AirFlash.AppDir/
├── AppRun                     # dispatches GUI vs CLI, fills runtime locations
├── .DirIcon / airflash-cli.png# icon used by appimagetool and desktops
├── airflash-cli.desktop      # CLI entry (X-AppImage-Version substituted)
├── VERSION                   # plain version text
└── usr/
    ├── bin/airflash-cli      # the CLI
    ├── bin/airflash-engine   # the JSONL engine, for the Python probe harness
    ├── share/applications/airflash-cli.desktop
    ├── share/icons/hicolor/512x512/apps/airflash-cli.png
    ├── share/doc/airflash-cli/README
    └── share/licenses/airflash-cli/{LICENSE-GPLv3,LICENSE-COMMERCIAL.md}
```

The `full` variant adds `usr/bin/airflash-ui` (a launcher),
`usr/lib/airflash-ui/` (the published Avalonia app) and
`airflash-ui.desktop` with `Terminal=false`.

Running the AppImage without installing: `./AirFlash-<version>-x86_64.AppImage discover`.
`AppRun` exports `AIRFLASH_RUNTIME_DIR` when neither it nor `XDG_RUNTIME_DIR` is
set, so the CLI works from a file manager as well as from a terminal.

## Audio input

- `--source loopback` — **system audio**: the default PipeWire sink monitor,
  linked explicitly by monitor port (never the microphone). `--monitor NAME`
  selects another sink by node name, id or description. When `pw-record` is
  absent, `parec` records the PulseAudio default sink monitor instead; when
  neither helper exists, startup fails with guidance instead of streaming
  silence. Native capture runs at 48 kHz and is resampled to the streaming
  rate with queue-level drift correction, mirroring the WASAPI loopback.
- `--source simulated` — a deterministic stereo signal (220 Hz left / 330 Hz
  right, one second of tone then one second of silence). Reproducible output,
  which is what the automated tests assert on.
- `--source file --wav PATH` — loops a WAV file (16/24/32-bit integer or
  32-bit float PCM, mono or multichannel, any rate, resampled to the streaming
  rate, up to ten minutes).
- `--codec alac|pcm|auto` (default `auto`) — ALAC is preferred and the sender
  retries once with the other codec when the audio SETUP is rejected, so a
  receiver that only accepts one of them still connects.
- The capture queue, resampling, equalizer, master gain, underrun silence
  padding and scheduler recovery mirror the Windows loopback, so behaviour and
  metrics are comparable across platforms.
- `airflash-cli status`, `logs` and the `capture_metrics` events report
  `input_rate`, `underrun_packets`, `dropped_frames` and queue-age percentiles
  for the selected input.

On virtual machines without a real audio clock the PipeWire graph can run
faster than wall-clock time; the sender then drops the excess to keep latency
bounded (`dropped_frames` grows while `capture_frames` outpaces the send
schedule). That is the graph outrunning time, not lost audio, and does not
happen on hardware with a real ALSA clock.

## Known limitations

- The graphical panel is new; it is not yet feature-complete against the Windows
  UI (see below) and has no tray icon on every desktop yet.
- `discover` implements a minimal mDNS querier: PTR/SRV/A/TXT parsing with
  compression pointers, no continuous browsing, no link-local IPv6 answers and
  no known-answer suppression. It is a CLI query, not a general DNS-SD stack.
- PTP timing needs privileged ports 319/320: grant `cap_net_bind_service` (or
  lower `net.ipv4.ip_unprivileged_port_start`). Without PTP, HomePods reject
  the audio stream even though the session connects.
- Equalizer control is available in the engine but is not exposed on the CLI.
- HomePod hardware qualification on Linux: a HomePod mini streams system audio
  (PipeWire monitor), WAV file and simulated signal end to end at 10% receiver
  volume with clean feedback; see the 0.4.1 release notes.
- No code signing or update channel for the AppImage; verify `SHA256SUMS.txt`
  out of band.
- Only x86_64 is packaged; the build script accepts other targets, but they are
  untested.

## Tests

```bash
dotnet test desktop/AirFlash.Tests/AirFlash.Tests.csproj
dotnet test desktop/AirFlash.UI.Tests/AirFlash.UI.Tests.csproj
cargo test --features cli --manifest-path native/airflash-engine/Cargo.toml
cargo clippy --all-targets --features cli --manifest-path native/airflash-engine/Cargo.toml -- -D warnings
AIRFLASH_BUILD_APPIMAGE=1 python -m pytest tests/test_linux_packaging.py
```

The Rust suite includes the headless CLI lifecycle tests (daemon start, fake
receiver streaming, control commands, graceful stop, failure without orphans,
SIGTERM handling, config files, argument validation) and the simulated audio
input tests.
