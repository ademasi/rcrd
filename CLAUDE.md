# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

rcrd is a passive call recorder for PipeWire that captures audio from VoIP applications (Teams, Zoom, Meet, etc.) without rerouting existing streams. It taps the default PipeWire sink monitor (remote audio) and microphone (local voice), mixes them, and writes to an OGG/Opus file. Optionally records one window (sway) to an MKV with the same audio. Plain terminal output: start-up lines, a two-line live status (status + key hints) redrawn in place, log lines for events; keys m/b/q. Follows `~/git/tui-guideline/GUIDELINE.md` (palette colors, glyphs, message style).

## Build Commands

```bash
cargo build --release          # Build release binary at target/release/rcrd
cargo build                    # Debug build
cargo run -- [OPTIONS]         # Build and run with arguments
```

## Architecture

```
main.rs       - Entry point, CLI args (clap), device/window selection, start-up and final lines
session.rs    - Recording loop: keys, follow-mode labels, the two live lines (status + hints)
term.rs       - Terminal layer: palette styles, raw-mode keys, numbered prompts, in-place redraw, signals
window.rs     - Window recording: wl-mirror fork mirrors the toplevel onto a headless sway output, wl-screenrec records it, ffmpeg muxes the audio in
config.rs     - Load config from ~/.config/rcrd/config.json
devices.rs    - Enumerate PipeWire audio devices and detect defaults via pw-dump / pw-metadata
ffmpeg.rs     - Spawn FFmpeg, construct filter graphs, mic volume control via command file
output.rs     - Filename generation (timestamp + optional --name label)
state.rs      - SharedState for thread-safe log sharing
util.rs       - Constants (audio params, timing), human_time, graceful child stop
```

## Key Patterns

- **Device Selection:** Default sink/source are auto-selected at launch; `--pick` asks with numbered prompts (also the fallback if detection fails). Auto-selected devices follow the system default mid-recording (mic opened as pulse `default`, monitor as `@DEFAULT_MONITOR@` with its `target.object` cleared via pw-metadata); explicitly chosen devices stay pinned
- **Thread Safety:** Shared state uses `Arc<Mutex<T>>` for FFmpeg log sharing
- **Mic Control:** Dynamic volume via FFmpeg's `asendcmd` filter writing to a FIFO file
- **Audio Mixing:** FFmpeg's `amix` filter with `duration=longest` keeps mic and monitor in sync
- **Device Detection:** Parses `pw-dump` JSON output to enumerate all audio devices and find defaults (`default.audio.*` preferred over the stale `default.configured.*`)
- **Window Recording (`--window`):** sway only. `swaymsg create_output` makes a headless output at the window's size (30 Hz, placed at x=10000, `no_focus` rule so focus never moves), `wl-mirror --fullscreen-output <it> toplevel:<id>` mirrors the window there (works for hidden windows), `wl-screenrec -o <it>` records it to `<base>.video.mkv`; at the end the OGG audio is stream-copied in to `<base>.mkv` and the output is unplugged. A spec with no match yet waits (newest matching window wins; `s` starts without it). Needs the toplevel-capture fork (`wl-mirror --list-toplevels`)
- **Terminal output:** never full-screen. `●` accent for the tool, blue for in-progress, green `✓ done:`, red `✗ failed:`, yellow for warnings, dim for secondary; lowercase fragments, ` · ` separators, `→` destinations

## External Dependencies

- **ffmpeg** with libopus codec
- **pw-dump** / **pw-metadata** (from pipewire) for device enumeration, default tracking, and stream re-targeting
- For `--window`: **sway**, the **wl-mirror** toplevel-capture fork (package `wl-mirror-toplevel`), **wl-screenrec** (or wf-recorder)

## Configuration

Location: `~/.config/rcrd/config.json`
- `file_prefix` - Output filename prefix (default: "rcrd-call-")
