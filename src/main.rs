mod audio_mix;
mod config;
mod devices;
mod ffmpeg;
mod output;
mod state;
mod transcript;
mod ui;
mod util;

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use clap::Parser;
use serde::Serialize;

use crate::audio_mix::{setup_mix_source, TranscriptionMix};
use crate::config::{config_path, load_config, Config};
use crate::devices::detect_defaults;
use crate::ffmpeg::{prepare_mic_control, spawn_ffmpeg};
use crate::output::{default_output_name, git_revision};
use crate::state::SharedState;
use crate::transcript::start_transcriber;
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

    /// Path to whisper-stream binary (overrides config).
    #[arg(long)]
    whisper_stream: Option<PathBuf>,
}

#[derive(Serialize)]
pub struct Marker {
    timestamp: f64,
    note: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cfg_path = config_path();
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!(
                "Warning: failed to load config {}: {}. Using defaults.",
                cfg_path.display(),
                e
            );
            Config::default()
        }
    };
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
    let language_str = args
        .lang
        .or(cfg.language.clone())
        .unwrap_or_else(|| "en".into());
    let shared = SharedState::new(language_str);
    let whisper_model = args.model.or(cfg.whisper_model.clone());
    let whisper_stream_bin = args
        .whisper_stream
        .or(cfg.whisper_stream_path.clone());
    let backend = args
        .backend
        .or(Some(cfg.backend.clone()))
        .unwrap_or_else(|| "vulkan".into());
    let model_exists = whisper_model
        .as_ref()
        .map(|p| p.exists())
        .unwrap_or(false);
    let stream_exists = whisper_stream_bin
        .as_ref()
        .map(|p| p.exists())
        .unwrap_or(false);
    let want_transcript = model_exists && stream_exists;
    let whisper_threads = transcription::DEFAULT_THREADS;

    if args.debug {
        println!("Debug mode enabled.");
        println!("Sink: {}", sink);
        println!("Monitor: {}", monitor);
        println!("Mic: {:?}", source_name);
        println!("Output: {}", outfile.display());
        println!(
            "Whisper model: {:?} (exists: {})",
            whisper_model, model_exists
        );
        println!(
            "whisper-stream: {:?} (exists: {})",
            whisper_stream_bin, stream_exists
        );
        println!("Whisper backend: {}", backend);
        if let Ok(lang) = shared.language.lock() {
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
        shared.logs.clone(),
        args.debug,
    )?;

    // Start transcription supervisor if a model and binary are provided
    let mut transcript_handle = None;
    let mut mix_handle: Option<TranscriptionMix> = None;
    if want_transcript {
        let trans_source = if let Some(mix) =
            setup_mix_source(&monitor, source_name.as_deref(), shared.logs.clone())
        {
            let src = mix.source.clone();
            mix_handle = Some(mix);
            src
        } else {
            monitor.clone()
        };

        if let (Some(model_path), Some(bin_path)) =
            (whisper_model.clone(), whisper_stream_bin.clone())
        {
            transcript_handle = Some(start_transcriber(
                model_path,
                bin_path,
                trans_source,
                shared.language.clone(),
                shared.transcript.clone(),
                shared.logs.clone(),
                shared.transcription_active.clone(),
                shared.transcription_stop.clone(),
                shared.base_offset_ms.clone(),
                shared.transcription_reset.clone(),
                whisper_threads,
            ));
        }
    } else if let Ok(mut logs) = shared.logs.lock() {
        if !model_exists {
            logs.push("Transcription disabled: whisper model not found".into());
        }
        if !stream_exists {
            logs.push("Transcription disabled: whisper-stream binary not found".into());
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
        markers: Vec::new(),
        shared: shared.clone(),
        transcription_active: false,
        transcription_available: want_transcript,
        whisper_model,
    };

    let res = run_app(state, &mut child);

    // Ensure FFmpeg is dead
    ensure_child_stopped(&mut child);
    shared.transcription_stop.store(true, Ordering::Relaxed);
    if let Some(handle) = transcript_handle {
        let _ = handle.join();
    }
    drop(mix_handle); // unload loopback modules if created

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
    let transcript = match state.shared.transcript.lock() {
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

use crate::util::{format_timecode, transcription};
