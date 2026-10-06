mod config;
mod devices;
mod ffmpeg;
mod output;
mod session;
mod state;
mod term;
mod util;
mod window;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use nix::sys::signal::Signal;
use serde::Serialize;

use crate::config::{Config, config_path, load_config};
use crate::devices::{
    AudioDevice, DeviceInfo, enumerate_devices, find_capture_stream, find_device,
    list_devices, short_describe, unpin_stream,
};
use crate::ffmpeg::{FfmpegConfig, prepare_mic_control, spawn_ffmpeg};
use crate::output::default_output_name;
use crate::session::Session;
use crate::state::SharedState;
use crate::term::{Live, Style, say};
use crate::util::{human_time, last_logs, push_log, stop_child};
use crate::window::{Toplevel, WindowRecorder, find_toplevel, list_toplevels};

/// Record a call (Teams, Zoom, etc.) by tapping the current PipeWire sink monitor and microphone.
///
/// Keys while recording: m mute/unmute the mic, b add a marker, q (or Ctrl-C) stop.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Output file path (default: rcrd-call-YYYYmmdd-HHMMSS.ogg)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Label appended to the generated filename, e.g. --name "meeting paul"
    /// gives rcrd-call-YYYYmmdd-HHMMSS-meeting-paul.ogg. Ignored with --output.
    #[arg(short, long)]
    name: Option<String>,

    /// Stop after this many seconds (omit to record until q or Ctrl-C)
    #[arg(short, long)]
    duration: Option<u32>,

    /// PipeWire sink node name to tap (monitor side). Defaults to current default sink.
    #[arg(long)]
    sink: Option<String>,

    /// PipeWire source node name to tap (microphone). Defaults to current default source.
    #[arg(long)]
    source: Option<String>,

    /// Do not record microphone; capture only the remote/output side.
    #[arg(long, default_value_t = false)]
    no_mic: bool,

    /// Choose the devices from a list instead of auto-selecting the defaults.
    #[arg(short, long, default_value_t = false)]
    pick: bool,

    /// Also record a window (sway) to <output>.mkv. SPEC is matched like
    /// wl-mirror's toplevel targets (identifier, app_id, title) or as a
    /// substring; if no window matches yet, rcrd waits for it to open. Omit
    /// SPEC to choose from a list.
    #[arg(short, long, value_name = "SPEC", num_args = 0..=1, default_missing_value = "")]
    window: Option<String>,

    /// List the windows that --window can record and exit.
    #[arg(long, default_value_t = false)]
    list_windows: bool,

    /// Debug mode: print the FFmpeg command and its output, no live display.
    #[arg(long, default_value_t = false)]
    debug: bool,
}

#[derive(Serialize)]
pub struct Marker {
    timestamp: f64,
    note: String,
}

fn main() -> Result<()> {
    term::install_signal_handlers();
    let style = Style::detect();
    let args = Args::parse();

    if args.list_windows {
        for t in list_toplevels()? {
            say(format!("{}\t{}\t{}", t.identifier, t.app_id, t.title));
        }
        return Ok(());
    }

    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            say(style.warn(&format!(
                "config {} not loaded: {e}, using defaults",
                config_path().display()
            )));
            Config::default()
        }
    };

    let outfile = args
        .output
        .clone()
        .unwrap_or_else(|| default_output_name(cfg.file_prefix.as_str(), args.name.as_deref()));

    // Devices: CLI args or the active PipeWire defaults, verified against the
    // nodes that exist. --pick asks instead, and is the fallback when
    // auto-detection fails and no devices were given explicitly.
    let info = enumerate_devices()?;
    if info.sinks.is_empty() {
        return Err(anyhow!(
            "no audio sinks found. Is PipeWire running?\n\
             Try: --sink <name> --source <name> to specify devices manually."
        ));
    }
    let Some((sink, source_name)) = select_devices(&args, &info, &style)? else {
        return Ok(()); // picker cancelled
    };

    // Auto-detected devices follow the system default while recording (dock,
    // headset, ...); explicitly chosen devices stay pinned.
    let follow_sink = !args.pick && args.sink.is_none();
    let follow_mic = !args.pick && args.source.is_none() && source_name.is_some();
    let follows = |f: bool| if f { style.dim(" · follows default") } else { String::new() };

    say(format!(
        "{} rcrd → {}",
        style.accent("●"),
        style.bold(&outfile.display().to_string())
    ));
    let width = term::width();
    say(term::truncate(
        &format!("  {}  {}{}", style.dim("out"), short_describe(&info.sinks, &sink), follows(follow_sink)),
        width,
    ));
    say(term::truncate(&format!(
        "  {}  {}{}",
        style.dim("mic"),
        source_name
            .as_deref()
            .map_or_else(|| style.dim("none"), |s| short_describe(&info.sources, s)),
        follows(follow_mic)
    ), width));
    // Window to record alongside, if asked. A spec that matches nothing yet
    // waits for the window to appear (e.g. start rcrd, then join the call).
    let video_final = outfile.with_extension("mkv");
    let window_target = match args.window.as_deref() {
        None => None,
        Some("") => {
            let list = list_toplevels()?;
            if list.is_empty() {
                say(style.warn("no windows to record"));
                None
            } else {
                let items: Vec<String> = list.iter().map(Toplevel::label).collect();
                term::choose(&style, "window", &items, None, Some("no window"))?
                    .map(|i| list[i].clone())
            }
        }
        Some(spec) => match find_toplevel(&list_toplevels()?, spec)?.cloned() {
            Some(t) => Some(t),
            None => {
                let mut live = Live::new(term::stdout_is_tty());
                match window::wait_for_window(spec, &style, &mut live)? {
                    window::Waited::Found(t) => Some(t),
                    window::Waited::Skipped => None,
                    window::Waited::Quit => return Ok(()),
                }
            }
        },
    };
    if let Some(t) = &window_target {
        say(term::truncate(
            &format!("  {}  {} → {}", style.dim("win"), t.label(), style.dim(&video_final.display().to_string())),
            width,
        ));
    }
    let sink_label = short_describe(&info.sinks, &sink);
    let mic_label = source_name.as_deref().map(|s| short_describe(&info.sources, s));

    let monitor = if follow_sink { "@DEFAULT_MONITOR@".to_string() } else { format!("{sink}.monitor") };
    // A pulse stream opened without a device name gets no fixed target, so
    // WirePlumber moves it whenever the default source changes.
    let mic_input = source_name
        .as_ref()
        .map(|s| if follow_mic { "default".to_string() } else { s.clone() });

    let mic_cmd_path = if source_name.is_some() { Some(prepare_mic_control()?) } else { None };
    let shared = SharedState::new();

    check_disk_space(&outfile, MIN_DISK_SPACE_MB)?;

    // Video first (its setup takes a few seconds), then audio.
    let video = window_target.and_then(|t| {
        match WindowRecorder::start(t, outfile.with_extension("video.mkv"), shared.logs.clone()) {
            Ok(v) => {
                say(format!(
                    "  {}  {} {}x{} · {}",
                    style.dim("win"),
                    style.accent2("▶"),
                    v.size.0,
                    v.size.1,
                    style.dim(v.recorder_name)
                ));
                Some(v)
            }
            Err(e) => {
                say(style.err(&format!("✗ window recording not started: {e:#}")));
                None
            }
        }
    });

    let ffmpeg_config = FfmpegConfig {
        monitor: &monitor,
        mic: mic_input.as_deref(),
        mic_cmd_path: mic_cmd_path.as_deref(),
        outfile: &outfile,
        duration: args.duration,
        debug: args.debug,
    };
    let mut child = spawn_ffmpeg(ffmpeg_config, shared.logs.clone())?;
    if follow_sink {
        unpin_monitor_stream(child.id(), shared.logs.clone());
    }

    if args.debug {
        say(format!("debug: monitor {monitor}"));
        let _ = child.wait();
        if let Some(v) = video {
            let _ = v.stop();
        }
        return Ok(());
    }

    let mut session = Session {
        start_time: Instant::now(),
        duration: args.duration.map(|d| Duration::from_secs(u64::from(d))),
        mic_muted: false,
        mic_cmd_file: mic_cmd_path,
        sink_label,
        mic_label,
        follow_sink,
        follow_mic,
        sinks: info.sinks,
        sources: info.sources,
        markers: Vec::new(),
        ffmpeg_died: false,
        video,
        video_failed: false,
    };

    let mut live = Live::new(term::stdout_is_tty());
    let res = session::run(&mut session, &mut child, &mut live, &style);
    drop(live);

    // SIGTERM lets FFmpeg flush and finalize the OGG container.
    stop_child(&mut child, Signal::SIGTERM, Duration::from_secs(3));
    if let Some(path) = &session.mic_cmd_file {
        let _ = std::fs::remove_file(path);
    }
    let elapsed = human_time(session.start_time.elapsed());

    match &res {
        Err(e) => say(style.err(&format!("✗ failed: {e:#}"))),
        Ok(()) if session.ffmpeg_died => {
            say(style.err(&format!("✗ failed: ffmpeg exited early · {}", outfile.display())));
            for l in last_logs(&shared.logs, 8) {
                say(format!("  {}", style.dim(&l)));
            }
        }
        Ok(()) => {
            let markers = match session.markers.len() {
                0 => String::new(),
                1 => " · 1 marker".to_string(),
                n => format!(" · {n} markers"),
            };
            say(format!(
                "{} done: {} · {elapsed}{markers}",
                style.ok("✓"),
                outfile.display()
            ));
        }
    }

    if !session.markers.is_empty() {
        let marker_file = outfile.with_extension("json");
        match File::create(&marker_file)
            .map_err(anyhow::Error::from)
            .and_then(|f| serde_json::to_writer_pretty(f, &session.markers).map_err(Into::into))
        {
            Ok(()) => say(format!("  markers → {}", style.dim(&marker_file.display().to_string()))),
            Err(e) => say(style.warn(&format!("✗ markers not saved: {e}"))),
        }
    }

    if let Some(v) = session.video.take() {
        let failed = session.video_failed;
        let size = format!("{}x{}", v.size.0, v.size.1);
        let video_only = v.stop();
        if failed {
            for l in last_logs(&shared.logs, 6) {
                say(format!("  {}", style.dim(&l)));
            }
        }
        let video_ok = video_only.metadata().map(|m| m.len() > 0).unwrap_or(false);
        let audio_ok = outfile.metadata().map(|m| m.len() > 0).unwrap_or(false);
        if video_ok && audio_ok {
            match window::mux(&video_only, &outfile, &video_final) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&video_only);
                    say(format!("{} done: {} · {size}", style.ok("✓"), video_final.display()));
                }
                Err(e) => say(style.warn(&format!(
                    "✗ video kept without audio: {} ({e})",
                    video_only.display()
                ))),
            }
        } else if video_ok {
            say(style.warn(&format!("✗ video kept without audio: {}", video_only.display())));
        } else {
            let _ = std::fs::remove_file(&video_only);
            say(style.err("✗ failed: no video was recorded"));
        }
    }

    Ok(())
}

/// Resolve sink and mic from the CLI, the active defaults, or the picker.
/// `None` means the user cancelled the picker.
fn select_devices(
    args: &Args,
    info: &DeviceInfo,
    style: &Style,
) -> Result<Option<(String, Option<String>)>> {
    if !args.pick || args.debug {
        if let Some(name) = &args.sink
            && find_device(&info.sinks, name).is_none() {
                return Err(anyhow!(
                    "sink '{name}' not found. Available sinks:\n{}",
                    list_devices(&info.sinks)
                ));
            }
        if let Some(name) = &args.source
            && find_device(&info.sources, name).is_none() {
                return Err(anyhow!(
                    "source '{name}' not found. Available sources:\n{}",
                    list_devices(&info.sources)
                ));
            }
        let sink = args
            .sink
            .clone()
            .or_else(|| available_default(&info.sinks, info.defaults.sink.as_deref(), "sink", style));
        let source = if args.no_mic {
            None
        } else {
            args.source.clone().or_else(|| {
                available_default(&info.sources, info.defaults.source.as_deref(), "mic", style)
            })
        };
        match (sink, source) {
            (Some(s), src) if args.no_mic || src.is_some() => return Ok(Some((s, src))),
            _ if args.debug || args.sink.is_some() || args.source.is_some() => {
                return Err(anyhow!(
                    "could not detect the default devices. Pass --sink/--source explicitly, \
                     or run with --pick to choose."
                ));
            }
            _ => say(style.warn("default devices not detected, choose them:")),
        }
    }
    pick_devices(info, style)
}

fn pick_devices(info: &DeviceInfo, style: &Style) -> Result<Option<(String, Option<String>)>> {
    let default_sink = info.defaults.sink.as_deref().and_then(|d| index_of(&info.sinks, d));
    let items: Vec<String> = info.sinks.iter().map(ToString::to_string).collect();
    let Some(si) = term::choose(style, "out", &items, default_sink, None)? else {
        return Ok(None);
    };
    let default_source = info.defaults.source.as_deref().and_then(|d| index_of(&info.sources, d));
    let items: Vec<String> = info.sources.iter().map(ToString::to_string).collect();
    let mi = term::choose(style, "mic", &items, default_source, Some("no mic"))?;
    Ok(Some((info.sinks[si].node_name.clone(), mi.map(|i| info.sources[i].node_name.clone()))))
}

fn index_of(list: &[AudioDevice], name: &str) -> Option<usize> {
    list.iter().position(|d| d.node_name == name)
}

/// The PipeWire default, but only if that node currently exists.
fn available_default(
    list: &[AudioDevice],
    name: Option<&str>,
    kind: &str,
    style: &Style,
) -> Option<String> {
    let name = name?;
    if find_device(list, name).is_some() {
        Some(name.to_string())
    } else {
        say(style.warn(&format!("default {kind} '{name}' is not available")));
        None
    }
}

/// pipewire-pulse pins the monitor capture to whichever sink was default when
/// FFmpeg connected. Clear that target once the stream appears so WirePlumber
/// moves it when the default sink changes (dock, headset, ...).
fn unpin_monitor_stream(pid: u32, logs: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(250));
            match find_capture_stream(pid, true) {
                Ok(Some(id)) => {
                    if let Err(e) = unpin_stream(id) {
                        push_log(&logs, format!("could not unpin monitor stream {id}: {e}"));
                    }
                    return;
                }
                Ok(None) => {}
                Err(e) => {
                    push_log(&logs, format!("{e}"));
                    return;
                }
            }
        }
        push_log(&logs, "monitor stream not found; it will not follow default sink changes");
    });
}

const MIN_DISK_SPACE_MB: u64 = 100;

fn check_disk_space(path: &Path, min_mb: u64) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stat = nix::sys::statvfs::statvfs(parent).context("failed to check disk space")?;
    let free_mb = (stat.blocks_available() as u64 * stat.block_size() as u64) / (1024 * 1024);
    if free_mb < min_mb {
        anyhow::bail!(
            "insufficient disk space: {free_mb} MB available, {min_mb} MB recommended. \
             Free up space or use --output to specify a different location."
        );
    }
    Ok(())
}
