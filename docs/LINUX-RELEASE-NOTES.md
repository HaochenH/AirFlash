# AirFlash 0.4.0 · Linux preview

Linux x86_64 and aarch64 · English and Simplified Chinese · AirPlay 2 sender

> **Preview.** The Linux slice reuses the native sender, pairing and encrypted RTP
> path that ships on Windows, but **system audio capture is not implemented on
> Linux yet** (PipeWire/PulseAudio). The headless CLI and the panel stream a
> deterministic test signal or a WAV file, which keeps the whole transport path
> real and testable. That limit is stated here, in `docs/LINUX.md`, and in the UI
> instead of being hidden.

## What is in this release

Four AppImages built by `packaging/linux/appimage/build-appimage.sh`, one pair
per architecture:

| Asset | Variant | Contains |
| --- | --- | --- |
| `AirFlash-0.4.0-x86_64.AppImage` | `backend` | `airflash-cli` + the JSONL `airflash-engine` |
| `AirFlash-0.4.0-x86_64-full.AppImage` | `full` | the above plus the Avalonia panel (`Terminal=false` desktop entry) |
| `AirFlash-0.4.0-aarch64.AppImage` | `backend` | same as the x86_64 backend image |
| `AirFlash-0.4.0-aarch64-full.AppImage` | `full` | same as the x86_64 full image |

In the full image, running the AppImage with no arguments opens the panel;
`AirFlash.AppImage discover` (or any CLI command) still drives the headless binary.
`SHA256SUMS.txt` covers all four; the images are not signed, so verify it out of band.

Both architectures were produced from one aarch64 host: the x86_64 pair by
cross-compiling with `TARGET=x86_64-unknown-linux-gnu` and assembling the image
from the official AppImage runtime, because `appimagetool` cannot run on a
foreign architecture.

## Features

- **Headless `airflash-cli`**: `discover`, `start` (foreground or `--daemon`),
  `stop`, `status`, `logs`, `volume`, `gain`, `pair`, `version`. Every command
  works without a graphical desktop, a tty or a service manager.
- **Background operation**: state, log and a Unix control socket live in
  `AIRFLASH_RUNTIME_DIR` (or `$XDG_RUNTIME_DIR/airflash`), never `/tmp`; SIGTERM
  becomes a graceful RTSP teardown; a single-instance lock prevents two daemons.
- **Graphical panel** (Avalonia): the Windows panel's layout, receiver list,
  per-device volume and mute, master volume, latency mode, equalizer with presets
  and live preview, diagnostics page, and settings pages for startup, stream
  format and standby.
- **Discovery** through the sender's own mDNS parser; the CLI reports receivers as
  JSON, and the panel consumes that JSON, so only one mDNS implementation exists.
- **Testable audio inputs**: `--source simulated` (deterministic 220 Hz left /
  330 Hz right tone, one second of silence per second) and `--source file`
  (WAV, resampled to the streaming rate).
- **`systemd --user`** units (plain plus a per-receiver template) that need no
  root, keep state in the per-user runtime directory, retry a few times on
  failure, and stop gracefully.
- **Repeatable packaging**: one script builds both variants on x86_64 and cross
  hosts, verifies the AppDir before packaging, and writes `SHA256SUMS.txt`.

## Verification

The release was verified without HomePod hardware: the Rust suite includes a
simulated AirPlay receiver (HAP/SRP, SETUP, RECORD, TEARDOWN, UDP media) that
drives the real CLI through daemon start, streaming, control commands, graceful
stop, failure without orphaned processes, SIGTERM, config files and argument
validation. The .NET suites cover the shared Core, the Linux service layer, the
discovery mapping and — on a headless Avalonia dispatcher — the panel itself. A
Linux workflow builds and tests everything on every push and pull request.

The aarch64 images were additionally exercised on the build host: the CLI
completes a daemon start, control and stop cycle against a local listener, every
shipped ELF is aarch64 (including the bundled .NET runtime), and the panel was
launched from the extracted image and confirmed to discover the AirPlay
receivers present on that network.

## Fixes after the first upload

- **The panel now fills its receiver list.** Discovery reconciliation blocked the
  UI thread on a semaphore and then on the reconcile's own continuations, which
  capture the UI `SynchronizationContext`; the dispatcher could never advance, so
  the panel opened and stayed on "Searching for receivers". The reconcile is
  awaited instead of blocked, matching the Windows view model.
- **A second launch exits cleanly.** The single-instance check ran after Avalonia
  had started and shut down a live dispatcher, which threw. The lock is now taken
  before the UI framework starts, so a duplicate launch raises the existing panel
  and returns.

Both are covered by new tests, including a headless dispatcher regression test.

## Known limitations

- No system audio capture on Linux yet (PipeWire/PulseAudio).
- No tray icon on every desktop environment; the window closing hides to the tray
  where the platform provides one.
- PTP timing needs ports 319/320, which unprivileged users cannot bind by
  default; use `timing: "ntp"` or lower `net.ipv4.ip_unprivileged_port_start`.
- Discovery is a query, not continuous browsing; no IPv6 link-local answers.
- HomePod hardware qualification has not been performed on Linux.
- The AppImages are not code-signed; verify `SHA256SUMS.txt` out of band.

## Upgrade notes

Nothing on Windows changes. The Windows MSI and executable continue to be built
by the existing Windows release workflow.

Full details: <https://github.com/HaochenH/AirFlash/blob/main/docs/LINUX.md>
