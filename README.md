# rcrd

Passive call recorder for PipeWire. It taps the default output monitor (remote audio) and the default microphone (local voice), mixes them, and writes an OGG/Opus file. Works with Teams, Zoom, Meet, etc., without rerouting existing streams. On sway it can also record one window alongside the audio.

## Requirements
- PipeWire with PulseAudio compatibility (for monitor/source names)
- `ffmpeg` (with `libopus`)
- `pw-dump` and `pw-metadata` (from the `pipewire` package) for device detection and tracking
- For `--window` only: sway, the toplevel-capture fork of [wl-mirror](https://github.com/ademasi/wl-mirror) (`wl-mirror --list-toplevels` must work), and `wl-screenrec` (or `wf-recorder`)

## Build
```bash
cargo build --release
# binary: target/release/rcrd

# optional: install to ~/.local/bin
install -Dm755 target/release/rcrd ~/.local/bin/rcrd
```

## Usage
- Record until `q` or Ctrl+C, with a label in the file name:
  ```bash
  rcrd --name "meeting paul"     # rcrd-call-YYYYmmdd-HHMMSS-meeting-paul.ogg
  ```
- Limit duration (seconds) or set the output path:
  ```bash
  rcrd --duration 600 --output ~/call.ogg
  ```
- Record only the remote side (skip mic):
  ```bash
  rcrd --no-mic
  ```
- Choose the devices from a numbered list instead of the defaults:
  ```bash
  rcrd --pick
  ```
- Pin specific devices (monitor is `<sink>.monitor`):
  ```bash
  rcrd --sink <sink_node.name> --source <source_node.name>
  ```
- Also record a window to `<output>.mkv` (sway). The spec matches the window's app id or title; if nothing matches yet, rcrd waits for it to open:
  ```bash
  rcrd -w zoom -n "meeting paul"  # start, then join the call
  rcrd -w                         # choose from a list
  rcrd --list-windows
  ```

While recording, rcrd prints a status line and the key hints, redrawn in place:

```
● rcrd → rcrd-call-20261006-160000-meeting-paul.ogg
  out  Ryzen HD Audio Controller Speaker · follows default
  mic  Ryzen HD Audio Controller Digital Microphone · follows default
● 0:42 · mic on air · ⚑ 1 · Ryzen HD Audio Controller Digital Microphone
m mute  b marker  q quit
```

Keys: `m` mute/unmute the mic, `b` add a marker, `q` (or Ctrl+C) stop.

## Behavior
- Default output name: `rcrd-call-YYYYmmdd-HHMMSS[-name].ogg`; the prefix comes from `file_prefix` in `~/.config/rcrd/config.json`.
- Auto-selected devices follow the system default during the recording: undocking or connecting a headset moves the capture with it. Devices given with `--sink`, `--source` or `--pick` stay pinned.
- Markers are saved next to the recording as `<output>.json`.
- Stops automatically if `--duration` is provided. Stopping always lets FFmpeg finalize the file.
- Mixing uses `amix` to keep remote and mic audio in sync; with `--no-mic` only the sink monitor is recorded.
- Window recording mirrors the window with wl-mirror onto a temporary headless sway output (never visible, focus untouched), records that output with GPU encoding, and muxes the audio in at the end. The OGG is kept as well.
