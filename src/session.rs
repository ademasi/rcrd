//! The recording session: keys, follow-mode device labels, and the two live
//! lines (status + key hints) that are redrawn while recording.

use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::Marker;
use crate::devices::{AudioDevice, current_defaults, enumerate_devices, short_describe};
use crate::ffmpeg::write_mic_volume;
use crate::term::{self, Key, Live, Style};
use crate::util::{human_time, ui as ui_const};
use crate::window::WindowRecorder;

pub struct Session {
    pub start_time: Instant,
    pub duration: Option<Duration>,
    pub mic_muted: bool,
    pub mic_cmd_file: Option<PathBuf>,
    /// Short device names shown on screen.
    pub sink_label: String,
    pub mic_label: Option<String>,
    /// Auto-selected devices follow the system default while recording.
    pub follow_sink: bool,
    pub follow_mic: bool,
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    pub markers: Vec<Marker>,
    pub ffmpeg_died: bool,
    pub video: Option<WindowRecorder>,
    pub video_failed: bool,
}

impl Session {
    pub fn elapsed(&self) -> String {
        human_time(self.start_time.elapsed())
    }

    /// Re-read the active defaults; returns the lines to log for changes.
    fn refresh_labels(&mut self, style: &Style) -> Vec<String> {
        let mut changes = Vec::new();
        let Ok(defaults) = current_defaults() else {
            return changes;
        };
        if self.follow_sink
            && let Some(name) = defaults.sink {
                let label = self.label_for(&name, true);
                if label != self.sink_label {
                    changes.push(format!("{} out → {label}", style.dim("↷")));
                    self.sink_label = label;
                }
            }
        if self.follow_mic
            && let Some(name) = defaults.source {
                let label = self.label_for(&name, false);
                if Some(&label) != self.mic_label.as_ref() {
                    changes.push(format!("{} mic → {label}", style.dim("↷")));
                    self.mic_label = Some(label);
                }
            }
        changes
    }

    fn label_for(&mut self, name: &str, sink: bool) -> String {
        let list = if sink { &self.sinks } else { &self.sources };
        if list.iter().any(|d| d.node_name == name) {
            return short_describe(list, name);
        }
        // A device we have not seen yet (e.g. a headset that just connected).
        if let Ok(info) = enumerate_devices() {
            self.sinks = info.sinks;
            self.sources = info.sources;
        }
        short_describe(if sink { &self.sinks } else { &self.sources }, name)
    }
}

/// Run until the user quits, the duration elapses, a stop signal arrives, or
/// FFmpeg exits.
pub fn run(s: &mut Session, child: &mut Child, live: &mut Live, style: &Style) -> Result<()> {
    let raw = if live.enabled && term::stdin_is_tty() { term::RawMode::enable() } else { None };
    let keys = raw.is_some();
    let poll = Duration::from_millis(ui_const::POLL_INTERVAL_MS);
    let mut last_defaults = Instant::now();
    let mut last_draw: Option<Instant> = None;
    let mut dirty = true;

    loop {
        let key = if keys {
            term::read_key(poll)
        } else {
            std::thread::sleep(poll);
            None
        };
        match key {
            Some(Key::Quit) => break,
            Some(Key::Mute) => {
                if let Some(path) = &s.mic_cmd_file {
                    s.mic_muted = !s.mic_muted;
                    let _ = write_mic_volume(path, if s.mic_muted { 0.0 } else { 1.0 });
                    let t = s.elapsed();
                    live.log(if s.mic_muted {
                        style.warn(&format!("mic muted · {t}"))
                    } else {
                        format!("mic on air · {t}")
                    });
                    dirty = true;
                }
            }
            Some(Key::Marker) => {
                let elapsed = s.start_time.elapsed();
                s.markers.push(Marker {
                    timestamp: elapsed.as_secs_f64(),
                    note: format!("Marker #{}", s.markers.len() + 1),
                });
                live.log(format!(
                    "{} marker {} · {}",
                    style.accent("⚑"),
                    s.markers.len(),
                    human_time(elapsed)
                ));
                dirty = true;
            }
            Some(Key::Start) | None => {}
        }

        if term::stop_requested() {
            break;
        }

        match child.try_wait() {
            Ok(Some(_)) => {
                // FFmpeg exiting on its own is only expected near a --duration
                // cutoff; anything else is a mid-recording failure.
                let expected = s
                    .duration
                    .is_some_and(|d| s.start_time.elapsed() + Duration::from_secs(2) >= d);
                if !expected {
                    s.ffmpeg_died = true;
                }
                break;
            }
            Ok(None) => {}
            Err(e) => return Err(e.into()),
        }

        if s.duration.is_some_and(|d| s.start_time.elapsed() >= d) {
            break;
        }

        if (s.follow_sink || s.follow_mic)
            && last_defaults.elapsed() >= Duration::from_millis(ui_const::DEFAULTS_POLL_MS)
        {
            last_defaults = Instant::now();
            for line in s.refresh_labels(style) {
                live.log(line);
                dirty = true;
            }
        }

        if let Some(v) = &mut s.video
            && !s.video_failed && !v.alive() {
                s.video_failed = true;
                live.log(style.err("✗ window recording stopped early"));
                dirty = true;
            }

        let due = last_draw.is_none_or(|t| t.elapsed() >= Duration::from_millis(ui_const::REDRAW_MS));
        if live.enabled && (dirty || due) {
            live.draw(&[status_line(s, style), hints(s, style)]);
            last_draw = Some(Instant::now());
            dirty = false;
        }
    }
    live.finish();
    Ok(())
}

fn status_line(s: &Session, st: &Style) -> String {
    let sep = st.dim(" · ");
    let mut timer = s.elapsed();
    if let Some(d) = s.duration {
        timer = format!("{timer} / {}", human_time(d));
    }
    let mut parts = vec![format!("{} {}", st.accent("●"), st.accent2(&timer))];
    parts.push(match (&s.mic_label, s.mic_muted) {
        (None, _) => st.dim("no mic"),
        (Some(_), true) => st.warn("mic muted"),
        (Some(_), false) => "mic on air".to_string(),
    });
    parts.push(format!("⚑ {}", s.markers.len()));
    if let Some(v) = &s.video {
        parts.push(if s.video_failed {
            st.err(&format!("✗ {}", v.target.app_id))
        } else {
            format!("{} {}", st.accent2("▶"), v.target.app_id)
        });
    }
    if let Some(mic) = &s.mic_label {
        parts.push(st.dim(mic));
    }
    parts.join(&sep)
}

fn hints(s: &Session, st: &Style) -> String {
    let mut chips: Vec<(&str, &str)> = Vec::new();
    if s.mic_label.is_some() {
        chips.push(("m", if s.mic_muted { "unmute" } else { "mute" }));
    }
    chips.push(("b", "marker"));
    chips.push(("q", "quit"));
    st.chips(&chips)
}
