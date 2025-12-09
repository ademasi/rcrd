use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
    Terminal,
};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

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
struct Marker {
    timestamp: f64,
    note: String,
}

struct RecorderState {
    start_time: Instant,
    duration: Option<Duration>,
    mic_muted: bool,
    mic_cmd_file: Option<PathBuf>,
    running: bool,
    output_file: PathBuf,
    monitor_source: String,
    mic_source: Option<String>,
    git_rev: Option<String>,
    audio_level: Arc<Mutex<f32>>,
    markers: Vec<Marker>,
    recent_logs: Arc<Mutex<Vec<String>>>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let defaults = detect_defaults().unwrap_or_default();

    // ... (defaults detection logic) ...
    let sink = args.sink.or(defaults.sink).ok_or_else(|| anyhow!("Could not detect default sink"))?;
    let source_name = if args.no_mic { None } else { Some(args.source.or(defaults.source).ok_or_else(|| anyhow!("Could not detect default source"))?) };
    let monitor = format!("{sink}.monitor");
    let outfile = args.output.unwrap_or_else(default_output_name);

    let mic_cmd_path = if source_name.is_some() { Some(prepare_mic_control()?) } else { None };
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
        println!("FFmpeg started. Press Ctrl+C to stop.");
        let _ = child.wait();
        return Ok(());
    }

    // Setup TUI
    // ...

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let state = RecorderState {
        start_time: Instant::now(),
        duration: args.duration.map(|d| Duration::from_secs(d as u64)),
        mic_muted: false,
        mic_cmd_file: mic_cmd_path,
        running: true,
        output_file: outfile,
        monitor_source: monitor,
        mic_source: source_name,
        git_rev: git_revision(),
        audio_level,
        markers: Vec::new(),
        recent_logs: recent_logs.clone(),
    };

    let res = run_app(&mut terminal, state, &mut child);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Ensure FFmpeg is dead
    if let Ok(Some(_)) = child.try_wait() {
        // already exited
    } else {
        let _ = child.kill();
        let _ = child.wait();
    }

    // Cleanup command file
    if let Some(path) = &res.as_ref().ok().and_then(|s| s.mic_cmd_file.as_ref()) {
        let _ = fs::remove_file(path);
    }

    // Save markers if any
    if let Ok(final_state) = &res {
        if !final_state.markers.is_empty() {
            let marker_file = final_state.output_file.with_extension("json");
            if let Ok(f) = File::create(&marker_file) {
                let _ = serde_json::to_writer_pretty(f, &final_state.markers);
                println!("Saved {} markers to {}", final_state.markers.len(), marker_file.display());
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

fn run_app<B: Backend>(
    terminal: &mut Terminal<B>,
    mut state: RecorderState,
    child: &mut Child,
) -> Result<RecorderState> {
    loop {
        terminal.draw(|f| ui(f, &state))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        state.running = false;
                    }
                    KeyCode::Char('m') => {
                        if let Some(cmd_path) = &state.mic_cmd_file {
                            state.mic_muted = !state.mic_muted;
                            let vol = if state.mic_muted { 0.0 } else { 1.0 };
                            let _ = write_mic_volume(cmd_path, vol);
                        }
                    }
                    KeyCode::Char('b') => {
                        let elapsed = state.start_time.elapsed().as_secs_f64();
                        state.markers.push(Marker {
                            timestamp: elapsed,
                            note: format!("Marker #{}", state.markers.len() + 1),
                        });
                    }
                    _ => {}
                }
            }
        }

        // Check if ffmpeg is still running
        match child.try_wait() {
            Ok(Some(status)) => {
                state.running = false;
                if !status.success() {
                    // Exited with error
                }
            }
            Ok(None) => {} // Still running
            Err(e) => return Err(e.into()),
        }

        if let Some(duration) = state.duration {
            if state.start_time.elapsed() >= duration {
                state.running = false;
            }
        }

        if !state.running {
            break;
        }
    }
    Ok(state)
}

fn ui(f: &mut ratatui::Frame, state: &RecorderState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(3), // Header
                Constraint::Length(5), // Info
                Constraint::Length(3), // Status
                Constraint::Length(3), // VU Meter
                Constraint::Min(0),    // Logs/Controls help
            ]
            .as_ref(),
        )
        .split(f.size());

    let title = Paragraph::new(" rcrd - Audio Recorder ")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    let elapsed = state.start_time.elapsed();
    let duration_text = if let Some(d) = state.duration {
        format!(
            "{:02}:{:02}:{:02} / {:02}:{:02}:{:02}",
            elapsed.as_secs() / 3600,
            (elapsed.as_secs() / 60) % 60,
            elapsed.as_secs() % 60,
            d.as_secs() / 3600,
            (d.as_secs() / 60) % 60,
            d.as_secs() % 60
        )
    } else {
        format!(
            "{:02}:{:02}:{:02}",
            elapsed.as_secs() / 3600,
            (elapsed.as_secs() / 60) % 60,
            elapsed.as_secs() % 60
        )
    };

    let info_text = format!(
        "File: {}
Sink: {}
Mic : {}
Rev : {}",
        state.output_file.file_name().unwrap_or_default().to_string_lossy(),
        state.monitor_source,
        state.mic_source.as_deref().unwrap_or("(disabled)"),
        state.git_rev.as_deref().unwrap_or("unknown")
    );
    let info_block = Block::default().title(" Info ").borders(Borders::ALL);
    let info = Paragraph::new(info_text).block(info_block).style(Style::default().fg(Color::White));
    f.render_widget(info, chunks[1]);

    let mic_status = if state.mic_source.is_some() {
        if state.mic_muted {
            Span::styled(" MUTED ", Style::default().bg(Color::Red).fg(Color::Black))
        } else {
            Span::styled(" ON AIR ", Style::default().bg(Color::Green).fg(Color::Black))
        }
    } else {
        Span::raw(" N/A ")
    };
    
    let status_line = Line::from(vec![
        Span::raw(" Status: "),
        Span::styled("RECORDING", Style::default().fg(Color::Red).add_modifier(Modifier::RAPID_BLINK)),
        Span::raw(" | Mic: "),
        mic_status,
        Span::raw(" | Time: "),
        Span::raw(duration_text),
        Span::raw(format!(" | Markers: {}", state.markers.len())),
    ]);
    
    let status_block = Block::default().borders(Borders::ALL);
    let status_p = Paragraph::new(status_line).block(status_block);
    f.render_widget(status_p, chunks[2]);

    // VU Meter
    let level = if let Ok(l) = state.audio_level.lock() { *l } else { 0.0 };
    // Map RMS dB roughly to 0-1 range. Silence is usually -60dB or less.
    let ratio = ((level + 60.0) / 60.0).clamp(0.0, 1.0) as f64;
    
    let gauge = Gauge::default()
        .block(Block::default().title(" Audio Level ").borders(Borders::ALL))
        .gauge_style(Style::default().fg(Color::Green))
        .ratio(ratio);
    f.render_widget(gauge, chunks[3]);

    let controls = Paragraph::new("Controls:\n[q] Quit\n[m] Toggle Mic Mute\n[b] Add Bookmark")
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().title(" Help ").borders(Borders::ALL));
    f.render_widget(controls, chunks[4]);
}

fn git_revision() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rev = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rev.is_empty() {
        None
    } else {
        Some(rev)
    }
}

// --- Helpers ---

#[derive(Default)]
struct Defaults {
    sink: Option<String>,
    source: Option<String>,
}

fn detect_defaults() -> Result<Defaults> {
    let output = Command::new("pw-dump")
        .output()
        .context("pw-dump failed (is pipewire-utils installed?)")?;
    if !output.status.success() {
        return Err(anyhow!("pw-dump exited with {}", output.status));
    }
    let root: Value = serde_json::from_slice(&output.stdout).context("pw-dump returned invalid JSON")?;
    let mut defaults = Defaults::default();
    let Some(array) = root.as_array() else { return Ok(defaults); };

    for obj in array {
        let Some(obj_type) = obj.get("type").and_then(Value::as_str) else { continue; };
        if obj_type != "PipeWire:Interface:Metadata" {
            continue;
        }
        let items = obj
            .get("metadata")
            .and_then(Value::as_array)
            .or_else(|| obj.get("info").and_then(|info| info.get("items")).and_then(Value::as_array));
        let Some(items) = items else { continue; };

        for item in items {
            let Some(key) = item.get("key").and_then(Value::as_str) else { continue; };
            match key {
                "default.audio.sink" | "default.configured.audio.sink" => {
                    if defaults.sink.is_none() {
                        defaults.sink = extract_name(item.get("value"));
                    }
                }
                "default.audio.source" | "default.configured.audio.source" => {
                    if defaults.source.is_none() {
                        defaults.source = extract_name(item.get("value"));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(defaults)
}

fn extract_name(val: Option<&Value>) -> Option<String> {
    let Some(val) = val else { return None; };
    if let Some(s) = val.as_str() {
        return Some(s.to_owned());
    }
    if let Some(obj) = val.as_object() {
        if let Some(name) = obj.get("name").and_then(Value::as_str) {
            return Some(name.to_owned());
        }
        if let Some(value) = obj.get("value").and_then(Value::as_str) {
            return Some(value.to_owned());
        }
    }
    None
}

fn default_output_name() -> PathBuf {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = now.as_secs() as i64;
    let tm = time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or_else(|_| time::OffsetDateTime::UNIX_EPOCH);
    let datetime = format!(
        "{:<04}{:<02}{:<02}-{:<02}{:<02}{:<02}",
        tm.year(),
        tm.month() as u8,
        tm.day(),
        tm.hour(),
        tm.minute(),
        tm.second()
    );
    PathBuf::from(format!("rcrd-call-{datetime}.ogg"))
}

fn prepare_mic_control() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("rcrd-mic");
    fs::create_dir_all(&dir)?;
    let cmd_path = dir.join(format!("mic-{}.cmd", std::process::id()));
    
    // Initialize with unmute command
    let mut f = File::create(&cmd_path)?;
    writeln!(f, "0.0 micvol volume 1.0")?;
    
    Ok(cmd_path)
}

fn write_mic_volume(cmd_path: &Path, volume: f32) -> Result<()> {
    let dir = cmd_path.parent().unwrap();
    let tmp_path = dir.join(format!("mic-{}.tmp", std::process::id()));
    {
        let mut f = File::create(&tmp_path)?;
        writeln!(f, "0.0 micvol volume {}", volume)?;
    }
    fs::rename(tmp_path, cmd_path)?;
    Ok(())
}

fn spawn_ffmpeg(
    monitor: &str,
    mic: Option<&str>,
    mic_cmd_path: Option<&Path>,
    outfile: &PathBuf,
    duration: Option<u32>,
    audio_level: Arc<Mutex<f32>>,
    recent_logs: Arc<Mutex<Vec<String>>>,
    debug: bool,
) -> Result<Child> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostdin", "-y"]); 
    if let Some(d) = duration {
        cmd.args(["-t", &d.to_string()]);
    }
    
    if !debug {
        // We remove -loglevel error so we get astats output
        // cmd.args(["-nostats"]); 
        // Actually we need stats or at least info to see astats? 
        // astats works with default loglevel.
        // If we use -nostats, we might miss it?
        // Let's rely on capturing stderr.
    }
    
    cmd.args(["-f", "pulse", "-i", monitor]);
    
    let mut filter_complex = String::new();
    
    if let Some(mic_name) = mic {
        cmd.args(["-f", "pulse", "-i", mic_name]);
        let mic_cmd = if let Some(cmd_path) = mic_cmd_path {
            format!("filename='{}',volume@micvol=volume=1.0", cmd_path.display())
        } else {
            "volume=1.0".to_string()
        };

        filter_complex.push_str(&format!(
            "[1:a]asendcmd={}[mic];[0:a][mic]amix=inputs=2:duration=longest:dropout_transition=3[mix];[mix]asplit[out_file][analysis];[analysis]astats=metadata=1:reset=1,anullsink",
            mic_cmd
        ));
        
        cmd.args(["-filter_complex", &filter_complex]);
        cmd.args(["-map", "[out_file]"]);
    } else {
        // [0:a] -> split -> [out_file]
        //                -> [analysis] -> astats
        filter_complex.push_str("[0:a]asplit[out_file][analysis];[analysis]astats=metadata=1:reset=1,anullsink");
        cmd.args(["-filter_complex", &filter_complex]);
        cmd.args(["-map", "[out_file]"]);
    }
    
    cmd.args([
        "-ac", "2",
        "-ar", "48000",
        "-c:a", "libopus",
        "-b:a", "128k",
    ]);
    cmd.arg(outfile);

    if debug {
        println!("FFmpeg command: {:?}", cmd);
        return Ok(cmd.spawn().context("failed to spawn ffmpeg")?);
    }

    // Capture stderr to parse astats
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().context("failed to spawn ffmpeg")?;
    
    let stderr = child.stderr.take().expect("failed to capture stderr");
    
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let re = Regex::new(r"RMS level:\s+([-0-9.]+)").unwrap();
        
        for line in reader.lines() {
            if let Ok(l) = line {
                // Store recent logs (last 10 lines)
                if let Ok(mut logs) = recent_logs.lock() {
                    if logs.len() >= 10 {
                        logs.remove(0);
                    }
                    logs.push(l.clone());
                }

                if let Some(caps) = re.captures(&l) {
                    if let Some(m) = caps.get(1) {
                        if let Ok(val) = m.as_str().parse::<f32>() {
                            if let Ok(mut lock) = audio_level.lock() {
                                *lock = val;
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(child)
}
