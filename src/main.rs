mod devices;
mod ffmpeg;
mod output;
mod ui;

use std::fs::File;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use clap::Parser;
use serde::Serialize;

use crate::devices::detect_defaults;
use crate::ffmpeg::{prepare_mic_control, spawn_ffmpeg};
use crate::output::{default_output_name, git_revision};
use crate::ui::{RecorderState, run_app};

/// Record a call (Teams, Zoom, etc.) by tapping the current PipeWire sink monitor and microphone.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Output file path (default: rcrd-call-YYYYmmdd-HHMMSS.ogg)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Stop after this many seconds (omit to record until Ctrl+C or 'q')
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

    /// Enable debug mode (prints FFmpeg command and output, disables TUI).
    #[arg(long, default_value_t = false)]
    debug: bool,
}

#[derive(Serialize)]
pub struct Marker {
    timestamp: f64,
    note: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let defaults = detect_defaults().unwrap_or_default();

    let sink = args
        .sink
        .or(defaults.sink)
        .ok_or_else(|| anyhow!("Could not detect default sink"))?;
    let source_name = if args.no_mic {
        None
    } else {
        Some(
            args.source
                .or(defaults.source)
                .ok_or_else(|| anyhow!("Could not detect default source"))?,
        )
    };
    let monitor = format!("{sink}.monitor");
    let outfile = args.output.unwrap_or_else(default_output_name);

    let mic_cmd_path = if source_name.is_some() {
        Some(prepare_mic_control()?)
    } else {
        None
    };
    let audio_level = Arc::new(Mutex::new(0.0));
    let recent_logs = Arc::new(Mutex::new(Vec::new()));

    if args.debug {
        println!("Debug mode enabled.");
        println!("Sink: {}", sink);
        println!("Monitor: {}", monitor);
        println!("Mic: {:?}", source_name);
        println!("Output: {}", outfile.display());
    }

    let mut child = spawn_ffmpeg(
        &monitor,
        source_name.as_deref(),
        mic_cmd_path.as_deref(),
        &outfile,
        args.duration,
        audio_level.clone(),
        recent_logs.clone(),
        args.debug,
    )?;

    if args.debug {
        let _ = child.wait();
        return Ok(());
    }

    let state = RecorderState {
        start_time: Instant::now(),
        duration: args.duration.map(|d| Duration::from_secs(d as u64)),
        mic_muted: false,
        mic_cmd_file: mic_cmd_path,
        running: true,
        output_file: outfile.clone(),
        monitor_source: monitor,
        mic_source: source_name,
        git_rev: git_revision(),
        audio_level,
        markers: Vec::new(),
        recent_logs,
    };

    let res = run_app(state, &mut child);

    // Ensure FFmpeg is dead
    ensure_child_stopped(&mut child);

    // Cleanup command file
    if let Some(path) = &res.as_ref().ok().and_then(|s| s.mic_cmd_file.as_ref()) {
        let _ = std::fs::remove_file(path);
    }

    // Save markers if any
    if let Ok(final_state) = &res {
        if !final_state.markers.is_empty() {
            let marker_file = final_state.output_file.with_extension("json");
            if let Ok(f) = File::create(&marker_file) {
                let _ = serde_json::to_writer_pretty(f, &final_state.markers);
                println!(
                    "Saved {} markers to {}",
                    final_state.markers.len(),
                    marker_file.display()
                );
            }
        }
    }

    if let Err(err) = res {
        eprintln!("Error: {:?}", err);
    } else {
        println!("Recording finished successfully.");
    }

    Ok(())
}

fn ensure_child_stopped(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
