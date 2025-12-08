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
```

## Usage
- Record indefinitely until Ctrl+C:
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

## Behavior
- Default output name: `rcrd-call-YYYYmmdd-HHMMSS.ogg`
- Stops automatically if `--duration` is provided, otherwise stop with Ctrl+C.
- Mixing uses `amix` to keep remote and mic audio in sync; when `--no-mic` is set, it records only the sink monitor.
