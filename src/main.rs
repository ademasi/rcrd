mod config;
mod devices;
mod ffmpeg;
mod output;
mod transcript;
mod ui;

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use clap::Parser;
use serde::Serialize;

use crate::config::load_config;
use crate::devices::detect_defaults;
use crate::ffmpeg::{prepare_mic_control, spawn_ffmpeg};
use crate::output::{default_output_name, git_revision};
use crate::transcript::{TransSegment, start_transcriber};
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

    /// Path to whisper.cpp model (gguf) for live transcription.
    #[arg(long)]
    model: Option<PathBuf>,

    /// Transcription language (e.g., en, fr).
    #[arg(long)]
    lang: Option<String>,

    /// Save transcript to CSV (timecode,text) when recording stops.
    #[arg(long, default_value_t = false)]
    save_transcript: bool,

    /// Whisper backend: vulkan or openblas (defaults to config or vulkan).
    #[arg(long)]
    backend: Option<String>,
}

#[derive(Serialize)]
pub struct Marker {
    timestamp: f64,
    note: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = load_config().unwrap_or_default();
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
    let outfile = args
        .output
        .unwrap_or_else(|| default_output_name(cfg.file_prefix.as_str()));

    let mic_cmd_path = if source_name.is_some() {
        Some(prepare_mic_control()?)
    } else {
        None
    };
    let audio_level = Arc::new(Mutex::new(ffmpeg::Levels::default()));
    let recent_logs = Arc::new(Mutex::new(Vec::new()));
    let transcript = Arc::new(Mutex::new(Vec::<TransSegment>::new()));
    let transcription_flag = Arc::new(AtomicBool::new(false));
    let transcription_stop = Arc::new(AtomicBool::new(false));
    let transcription_reset = Arc::new(AtomicBool::new(false));
    let base_offset_ms = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let whisper_model = args.model.or(cfg.whisper_model.clone());
    let backend = args
        .backend
        .or(Some(cfg.backend.clone()))
        .unwrap_or_else(|| "vulkan".into());
    let language = Arc::new(Mutex::new(
        args.lang
            .or(cfg.language.clone())
            .unwrap_or_else(|| "en".into()),
    ));
    let want_transcript = whisper_model.is_some();
    let whisper_threads = 8;

    if args.debug {
        println!("Debug mode enabled.");
        println!("Sink: {}", sink);
        println!("Monitor: {}", monitor);
        println!("Mic: {:?}", source_name);
        println!("Output: {}", outfile.display());
        println!("Whisper model: {:?}", whisper_model);
        println!("Whisper backend: {}", backend);
        if let Ok(lang) = language.lock() {
            println!("Language: {}", *lang);
        }
        if want_transcript {
            println!("Whisper threads: {}", whisper_threads);
        }
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
        want_transcript,
    )?;

    // Start transcription reader if a model is provided
    let mut transcript_handle = None;
    if want_transcript {
        if let Some(stdout) = child.stdout.take() {
            if let Some(model_path) = whisper_model.clone() {
                transcript_handle = Some(start_transcriber(
                    stdout,
                    model_path,
                    language.clone(),
                    transcript.clone(),
                    transcription_flag.clone(),
                    transcription_stop.clone(),
                    backend.clone(),
                    base_offset_ms.clone(),
                    transcription_reset.clone(),
                    whisper_threads,
                ));
            }
        }
    }

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
        transcript,
        transcription_active: false,
        transcription_flag,
        transcription_stop: transcription_stop.clone(),
        transcription_reset,
        base_offset_ms,
        language,
        whisper_model,
    };

    let res = run_app(state, &mut child);

    // Ensure FFmpeg is dead
    ensure_child_stopped(&mut child);
    transcription_stop.store(true, Ordering::Relaxed);
    if let Some(handle) = transcript_handle {
        let _ = handle.join();
    }

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
        if args.save_transcript {
            save_transcript_csv(final_state, &outfile)?;
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

fn save_transcript_csv(state: &RecorderState, outfile: &PathBuf) -> Result<()> {
    let transcript = match state.transcript.lock() {
        Ok(t) => t.clone(),
        Err(_) => Vec::new(),
    };
    if transcript.is_empty() {
        return Ok(());
    }
    let csv_path = outfile.with_extension("csv");
    let mut w = File::create(&csv_path)?;
    writeln!(w, "start,end,text")?;
    for seg in transcript {
        let start = format_timecode(seg.start_ms);
        let end = format_timecode(seg.end_ms);
        let text = seg.text.replace('"', "\"\"");
        writeln!(w, "{start},{end},\"{text}\"")?;
    }
    println!("Saved transcript to {}", csv_path.display());
    Ok(())
}

fn format_timecode(ms: i64) -> String {
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1000) % 60;
    let ms = ms % 1000;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}
