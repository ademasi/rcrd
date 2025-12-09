use std::io;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Gauge, Paragraph},
};

use crate::Marker;
use crate::ffmpeg::write_mic_volume;

pub struct RecorderState {
    pub start_time: Instant,
    pub duration: Option<Duration>,
    pub mic_muted: bool,
    pub mic_cmd_file: Option<PathBuf>,
    pub running: bool,
    pub output_file: PathBuf,
    pub monitor_source: String,
    pub mic_source: Option<String>,
    pub git_rev: Option<String>,
    pub audio_level: Arc<Mutex<f32>>,
    pub markers: Vec<Marker>,
    pub recent_logs: Arc<Mutex<Vec<String>>>,
}

pub fn run_app(mut state: RecorderState, child: &mut Child) -> Result<RecorderState> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut state, child);

    // Restore terminal even if run_loop fails
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = terminal.show_cursor();

    result.map(|_| state)
}

fn run_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut RecorderState,
    child: &mut Child,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, state))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        state.running = false;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
            Ok(Some(_status)) => {
                state.running = false;
            }
            Ok(None) => {}
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
    Ok(())
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
                Constraint::Length(3), // Controls
                Constraint::Min(4),    // Logs
            ]
            .as_ref(),
        )
        .split(f.size());

    let title = Paragraph::new(" rcrd - Audio Recorder ")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
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
        state
            .output_file
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        state.monitor_source,
        state.mic_source.as_deref().unwrap_or("(disabled)"),
        state.git_rev.as_deref().unwrap_or("unknown")
    );
    let info_block = Block::default().title(" Info ").borders(Borders::ALL);
    let info = Paragraph::new(info_text)
        .block(info_block)
        .style(Style::default().fg(Color::White));
    f.render_widget(info, chunks[1]);

    let mic_status = if state.mic_source.is_some() {
        if state.mic_muted {
            Span::styled(" MUTED ", Style::default().bg(Color::Red).fg(Color::Black))
        } else {
            Span::styled(
                " ON AIR ",
                Style::default().bg(Color::Green).fg(Color::Black),
            )
        }
    } else {
        Span::raw(" N/A ")
    };

    let status_line = Line::from(vec![
        Span::raw(" Status: "),
        Span::styled(
            "RECORDING",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::RAPID_BLINK),
        ),
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
    let level = if let Ok(l) = state.audio_level.lock() {
        *l
    } else {
        0.0
    };
    // Map RMS dB roughly to 0-1 range. Silence is usually -60dB or less.
    let ratio = ((level + 60.0) / 60.0).clamp(0.0, 1.0) as f64;

    let gauge = Gauge::default()
        .block(
            Block::default()
                .title(" Audio Level ")
                .borders(Borders::ALL),
        )
        .gauge_style(Style::default().fg(Color::Green))
        .ratio(ratio);
    f.render_widget(gauge, chunks[3]);

    let controls = Paragraph::new(
        "Controls: q / Esc / Ctrl+C = Quit   m = Mute/Unmute mic   b = Add marker\n\
         Files: output OGG in cwd; markers .json beside it\n\
         Devices: monitor from default sink, mic from default source (or --no-mic)",
    )
    .style(Style::default().fg(Color::Gray))
    .block(Block::default().title(" Controls ").borders(Borders::ALL));
    f.render_widget(controls, chunks[4]);

    let log_lines = if let Ok(logs) = state.recent_logs.lock() {
        logs.clone()
    } else {
        Vec::new()
    };
    let help = Paragraph::new(Text::raw(log_lines.join("\n")))
        .style(Style::default().fg(Color::Gray))
        .block(
            Block::default()
                .title(" FFmpeg Log (recent) ")
                .borders(Borders::ALL),
        );
    f.render_widget(help, chunks[5]);
}
