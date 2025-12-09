# rcrd

Passive call recorder for PipeWire. It taps the default output monitor (remote audio) and the default microphone (local voice), mixes them, and writes an OGG/Opus file. Works with Teams, Zoom, Meet, etc., without rerouting existing streams.

## Requirements
- PipeWire with PulseAudio compatibility (for monitor/source names)
- `ffmpeg` (with `libopus`)
- `pipewire-utils` (`pw-dump` for default device detection)

## Build
```bash
cargo build --release
# binary: target/release/rcrd

# optional: install to ~/.local/bin
install -Dm755 target/release/rcrd ~/.local/bin/rcrd
```

## Usage
- Record indefinitely until Ctrl+C (TUI shows elapsed time, VU meter, and logs):
  ```bash
  ./target/release/rcrd
  ```
- Limit duration (seconds):
  ```bash
  ./target/release/rcrd --duration 600 --output ~/call.ogg
  ```
- Record only the remote side (skip mic):
  ```bash
  ./target/release/rcrd --no-mic
  ```
- Override devices if auto-detection fails (monitor is `<sink>.monitor`):
  ```bash
  ./target/release/rcrd --sink <sink_node.name> --source <source_node.name>
  ```
- Controls in the TUI: `q` or `Esc` to quit, `Ctrl+C` to quit, `m` to toggle mic mute/unmute, `b` to add a marker.

## Behavior
- Default output name: `rcrd-call-YYYYmmdd-HHMMSS.ogg` (zero-padded, no spaces)
- Stops automatically if `--duration` is provided, otherwise stop with Ctrl+C.
- Mixing uses `amix` to keep remote and mic audio in sync; when `--no-mic` is set, it records only the sink monitor.
