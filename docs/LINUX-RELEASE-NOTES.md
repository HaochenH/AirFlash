# AirFlash 0.4.1 · Linux preview

Linux x86_64 · English and Simplified Chinese · AirPlay 2 sender

> **Preview.** 0.4.1 closes the two gaps that kept 0.4.0 quiet on real
> hardware: Linux now captures system audio, and the sender negotiates what
> HomePods actually require (ALAC-first with codec fallback, PTP timing with
> an explicit fallback warning). Qualified end to end against a HomePod mini.

## What is in this release

Two artifacts, both Linux x86_64 AppImages built by
`packaging/linux/appimage/build-appimage.sh`:

| Asset | Variant | Contains |
| --- | --- | --- |
| `AirFlash-0.4.1-x86_64.AppImage` | `backend` | `airflash-cli` + the JSONL `airflash-engine` |
| `AirFlash-0.4.1-x86_64-full.AppImage` | `full` | the above plus the Avalonia panel (`Terminal=false` desktop entry) |

In the full image, running the AppImage with no arguments opens the panel;
`AirFlash.AppImage discover` (or any CLI command) still drives the headless binary.

## New in 0.4.1

- **System audio capture on Linux** (`--source loopback`, the default):
  captures the default PipeWire sink monitor through explicitly linked monitor
  ports (the microphone can never be picked by fuzzy matching), with a
  `parec` fallback for PulseAudio-only hosts and a clear error when neither
  helper exists. `--monitor NAME` selects another sink by node name, id or
  description. Native 48 kHz capture is resampled to the streaming rate with
  queue-level drift correction, mirroring the WASAPI loopback.
- **HomePod playback fix**: the sender prefers ALAC (which HomePods require —
  they answer the audio SETUP with `400 Bad Request` for PCM) and retries once
  with the other codec when the audio SETUP is rejected. `--codec alac|pcm|auto`
  overrides the choice; `--device NAME` now picks up the receiver-advertised
  codecs and no longer counts the `_airplay`/`_raop` twins as ambiguous.
- **PTP timing that degrades honestly**: `--timing auto` (now the default)
  uses PTP when ports 319/320 can be bound and falls back to NTP with a
  `timing_fallback` warning otherwise. HomePods need PTP; grant the ports with
  `sudo setcap cap_net_bind_service=+ep` on the binary,
  `sysctl net.ipv4.ip_unprivileged_port_start=319`, or the documented
  `AmbientCapabilities` line in the systemd unit.
- **Panel**: system audio is a first-class source on Linux again, and a
  dispatcher deadlock that left the receiver list stuck on “Searching for
  receivers” is fixed (with a regression test).

## Verification

Qualified on hardware, not just against the simulated receiver: a HomePod mini
(`AudioAccessory5,1`) streamed the PipeWire monitor while a tone played
locally, plus a WAV file and the simulated signal — ALAC 44100, receiver
volume set to 10% and confirmed by readback, audio-onset markers on real
captured audio, thousands of packets with zero send errors and clean `feedback`
every 2 s. The Rust suite (fake-helper capture tests for PipeWire, Pulse
fallback, graph parsing and missing-helper guidance, plus an auto-defaults
localhost session), the .NET suites and the packaging checks all pass.

## Known limitations

- PTP timing needs privileged ports 319/320 (see above); without them HomePods
  reject the audio stream even though the session connects.
- No tray icon on every desktop environment; the window closing hides to the tray
  where the platform provides one.
- Discovery is a query, not continuous browsing; no IPv6 link-local answers.
- On virtual machines without a real audio clock the PipeWire graph can run
  faster than wall-clock time; the sender drops the excess to bound latency
  (visible as `dropped_frames`). This does not happen on hardware clocks.
- The AppImages are not code-signed; verify `SHA256SUMS.txt` out of band.

## Upgrade notes

Nothing on Windows changes. The Windows MSI and executable continue to be built
by the existing Windows release workflow. The example systemd config now uses
`"source": "loopback"` and `"timing": "auto"`; the unit file documents the
optional PTP capability lines.

Full details: <https://github.com/HaochenH/AirFlash/blob/main/docs/LINUX.md>
