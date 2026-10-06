//! Record a single Wayland window alongside the audio (sway only).
//!
//! No installed recorder can capture a toplevel directly, so the window is
//! mirrored by the wl-mirror toplevel-capture fork onto a temporary headless
//! sway output (placed far away, never visible, keyboard focus untouched) and
//! that output is recorded with wl-screenrec (or wf-recorder). Recorder, mirror
//! and headless output are all torn down when the recording stops.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::signal::Signal;
use serde_json::Value;

use crate::term::{self, Key, Live, Style};
use crate::util::{human_time, push_log, stop_child, video};

const MIRROR_APP_ID: &str = "at.yrlf.wl_mirror";
const MIRROR_TITLE: &str = "rcrd mirror";

#[derive(Clone, Debug)]
pub struct Toplevel {
    pub identifier: String,
    pub app_id: String,
    pub title: String,
}

impl Toplevel {
    /// `app_id — title`, the same label everywhere.
    pub fn label(&self) -> String {
        if self.title.is_empty() {
            self.app_id.clone()
        } else {
            format!("{} — {}", self.app_id, self.title)
        }
    }
}

/// Windows that can be captured, oldest first (wl-mirror --list-toplevels).
pub fn list_toplevels() -> Result<Vec<Toplevel>> {
    let out = Command::new("wl-mirror")
        .arg("--list-toplevels")
        .output()
        .context("wl-mirror not found")?;
    if !out.status.success() {
        bail!(
            "wl-mirror --list-toplevels failed: window recording needs the toplevel-capture \
             fork of wl-mirror (~/git/wl-mirror-toplevel-pkg)"
        );
    }
    let list = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            Some(Toplevel {
                identifier: f.next()?.to_string(),
                app_id: f.next().unwrap_or("").to_string(),
                title: f.next().unwrap_or("").to_string(),
            })
        })
        .filter(|t| t.app_id != MIRROR_APP_ID)
        .collect();
    Ok(list)
}

/// wl-mirror's own matching for `toplevel:<spec>`: identifier, app_id,
/// `app_id title`, title; the oldest window wins.
fn exact_match<'a>(list: &'a [Toplevel], spec: &str) -> Option<&'a Toplevel> {
    list.iter()
        .find(|t| t.identifier == spec)
        .or_else(|| list.iter().find(|t| t.app_id == spec))
        .or_else(|| list.iter().find(|t| format!("{} {}", t.app_id, t.title) == spec))
        .or_else(|| list.iter().find(|t| t.title == spec))
}

/// Case-insensitive substring of app_id or title, oldest first.
fn fuzzy_matches<'a>(list: &'a [Toplevel], spec: &str) -> Vec<&'a Toplevel> {
    let needle = spec.to_lowercase();
    list.iter()
        .filter(|t| {
            t.app_id.to_lowercase().contains(&needle) || t.title.to_lowercase().contains(&needle)
        })
        .collect()
}

/// The window `spec` names: an exact wl-mirror match, else a unique substring
/// match. `Ok(None)` when nothing matches (yet); an error when it is ambiguous.
pub fn find_toplevel<'a>(list: &'a [Toplevel], spec: &str) -> Result<Option<&'a Toplevel>> {
    if let Some(t) = exact_match(list, spec) {
        return Ok(Some(t));
    }
    let hits = fuzzy_matches(list, spec);
    match hits.len() {
        0 => Ok(None),
        1 => Ok(Some(hits[0])),
        _ => bail!(
            "'{spec}' matches several windows:\n{}",
            hits.iter().map(|t| format!("  {}", t.label())).collect::<Vec<_>>().join("\n")
        ),
    }
}

pub enum Waited {
    Found(Toplevel),
    /// The user pressed `s`: record without a window.
    Skipped,
    Quit,
}

/// Poll the window list until something matches `spec`. The newest match
/// wins, since the user is waiting for a window that is about to open.
pub fn wait_for_window(spec: &str, style: &Style, live: &mut Live) -> Result<Waited> {
    let raw = if live.enabled && term::stdin_is_tty() { term::RawMode::enable() } else { None };
    let started = Instant::now();
    let mut last_check = Instant::now() - Duration::from_secs(1);
    let hints = style.chips(&[("s", "start without it"), ("q", "quit")]);
    let poll = Duration::from_millis(crate::util::ui::POLL_INTERVAL_MS);
    loop {
        let key = if raw.is_some() {
            term::read_key(poll)
        } else {
            thread::sleep(poll);
            None
        };
        match key {
            Some(Key::Quit) => return Ok(Waited::Quit),
            Some(Key::Start) => return Ok(Waited::Skipped),
            _ => {}
        }
        if term::stop_requested() {
            return Ok(Waited::Quit);
        }
        if last_check.elapsed() >= Duration::from_secs(1) {
            last_check = Instant::now();
            let list = list_toplevels()?;
            let found = exact_match(&list, spec)
                .or_else(|| fuzzy_matches(&list, spec).last().copied())
                .cloned();
            if let Some(t) = found {
                live.finish();
                return Ok(Waited::Found(t));
            }
        }
        live.draw(&[
            format!(
                "{} waiting for a window matching '{spec}' {}",
                style.accent("●"),
                style.dim(&format!("· {}", human_time(started.elapsed())))
            ),
            hints.clone(),
        ]);
    }
}

// ── sway IPC ───────────────────────────────────────────────────────────────

fn swaymsg(args: &[&str]) -> Result<String> {
    let out = Command::new("swaymsg")
        .args(args)
        .output()
        .context("swaymsg not found: window recording needs sway")?;
    if !out.status.success() {
        bail!(
            "swaymsg {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn sway_tree() -> Result<Value> {
    serde_json::from_str(&swaymsg(&["-t", "get_tree"])?).context("parsing sway tree")
}

fn walk<'a>(node: &'a Value, f: &mut dyn FnMut(&'a Value)) {
    f(node);
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(Value::as_array) {
            for c in children {
                walk(c, f);
            }
        }
    }
}

fn str_prop<'a>(node: &'a Value, key: &str) -> &'a str {
    node.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Current size of the window in sway's tree (matched by app_id and title).
pub fn window_size(t: &Toplevel) -> Option<(u32, u32)> {
    let tree = sway_tree().ok()?;
    let mut size = None;
    walk(&tree, &mut |n| {
        if size.is_none() && str_prop(n, "app_id") == t.app_id && str_prop(n, "name") == t.title {
            let r = &n["rect"];
            if let (Some(w), Some(h)) = (r["width"].as_u64(), r["height"].as_u64()) {
                size = Some((w as u32, h as u32));
            }
        }
    });
    size
}

fn focused_con_id() -> Option<i64> {
    let tree = sway_tree().ok()?;
    let mut id = None;
    walk(&tree, &mut |n| {
        if n["focused"].as_bool() == Some(true) && matches!(str_prop(n, "type"), "con" | "floating_con")
        {
            id = n["id"].as_i64();
        }
    });
    id
}

fn output_names() -> Result<Vec<String>> {
    let v: Value = serde_json::from_str(&swaymsg(&["-t", "get_outputs"])?)?;
    Ok(v.as_array()
        .map(|a| a.iter().filter_map(|o| o["name"].as_str().map(String::from)).collect())
        .unwrap_or_default())
}

/// (focused, output) of the mirror window once it is mapped.
fn mirror_window_state() -> Option<(bool, String)> {
    let tree = sway_tree().ok()?;
    let mut state = None;
    let mut current_output = String::new();
    // Workspaces are children of outputs, so the output is known when we reach the window.
    fn visit(n: &Value, output: &mut String, state: &mut Option<(bool, String)>) {
        if str_prop(n, "type") == "output" {
            *output = str_prop(n, "name").to_string();
        }
        if str_prop(n, "app_id") == MIRROR_APP_ID && str_prop(n, "name") == MIRROR_TITLE {
            *state = Some((n["focused"].as_bool() == Some(true), output.clone()));
        }
        for key in ["nodes", "floating_nodes"] {
            if let Some(children) = n.get(key).and_then(Value::as_array) {
                for c in children {
                    visit(c, output, state);
                }
            }
        }
    }
    visit(&tree, &mut current_output, &mut state);
    state
}

fn has_command(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn pipe_stderr(child: &mut Child, prefix: &'static str, logs: Arc<Mutex<Vec<String>>>) {
    use std::io::{BufRead, BufReader};
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            for l in BufReader::new(stderr).lines().map_while(Result::ok) {
                push_log(&logs, format!("{prefix}: {l}"));
            }
        });
    }
}

// ── recorder ───────────────────────────────────────────────────────────────

pub struct WindowRecorder {
    pub target: Toplevel,
    pub size: (u32, u32),
    pub recorder_name: &'static str,
    pub video_path: PathBuf,
    output: String,
    mirror: Child,
    recorder: Child,
    done: bool,
}

impl WindowRecorder {
    pub fn start(target: Toplevel, video_path: PathBuf, logs: Arc<Mutex<Vec<String>>>) -> Result<Self> {
        let recorder_name = if has_command("wl-screenrec") {
            "wl-screenrec"
        } else if has_command("wf-recorder") {
            "wf-recorder"
        } else {
            bail!("no screen recorder found: install wl-screenrec (or wf-recorder)");
        };
        swaymsg(&["-t", "get_version"]).context("window recording needs sway")?;

        // Headless output at the window's size (even dimensions for the encoder).
        let (w, h) = window_size(&target).unwrap_or((1920, 1080));
        let size = ((w.min(video::MAX_WIDTH)) & !1, (h.min(video::MAX_HEIGHT)) & !1);

        let prev_focus = focused_con_id();
        let before = output_names()?;
        swaymsg(&["-q", "create_output"])?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let output = loop {
            if let Some(new) = output_names()?.into_iter().find(|o| !before.contains(o)) {
                break new;
            }
            if Instant::now() > deadline {
                bail!("sway did not create a headless output");
            }
            thread::sleep(Duration::from_millis(100));
        };

        let mode = format!("{}x{}@{}Hz", size.0, size.1, video::FPS);
        if let Err(e) = swaymsg(&[
            "--", "output", &output, "mode", "--custom", &mode, "position", "10000", "0", "bg",
            "#000000", "solid_color",
        ]) {
            let _ = swaymsg(&["output", &output, "unplug"]);
            return Err(e);
        }
        // Keep keyboard focus where it is when the mirror window maps.
        let _ = swaymsg(&[
            "--",
            "no_focus",
            &format!("[app_id=\"{MIRROR_APP_ID}\" title=\"{MIRROR_TITLE}\"]"),
        ]);

        let mut mirror = Command::new("wl-mirror")
            .args(["--fullscreen-output", &output, "--no-show-cursor", "-s", "fit"])
            .args(["--title", MIRROR_TITLE])
            .arg(format!("toplevel:{}", target.identifier))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn wl-mirror")?;
        pipe_stderr(&mut mirror, "wl-mirror", logs.clone());

        // Wait for the mirror window to land on the headless output.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(Some(status)) = mirror.try_wait() {
                let _ = swaymsg(&["output", &output, "unplug"]);
                bail!("wl-mirror exited ({status}) before capturing '{}'", target.label());
            }
            if let Some((focused, on)) = mirror_window_state()
                && on == output {
                    if focused
                        && let Some(id) = prev_focus {
                            let _ = swaymsg(&[&format!("[con_id={id}]"), "focus"]);
                        }
                    break;
                }
            if Instant::now() > deadline {
                push_log(&logs, "wl-mirror: window not seen on the headless output after 5s");
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let recorder = Command::new(recorder_name)
            .args(["-o", &output, "-f"])
            .arg(&video_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn {recorder_name}"));
        let mut recorder = match recorder {
            Ok(r) => r,
            Err(e) => {
                stop_child(&mut mirror, Signal::SIGTERM, Duration::from_secs(2));
                let _ = swaymsg(&["output", &output, "unplug"]);
                return Err(e);
            }
        };
        pipe_stderr(&mut recorder, recorder_name, logs);

        Ok(Self { target, size, recorder_name, video_path, output, mirror, recorder, done: false })
    }

    /// The recorder is still running.
    pub fn alive(&mut self) -> bool {
        matches!(self.recorder.try_wait(), Ok(None))
    }

    fn teardown(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        // SIGINT lets the recorder finalize its container.
        stop_child(&mut self.recorder, Signal::SIGINT, Duration::from_secs(5));
        stop_child(&mut self.mirror, Signal::SIGTERM, Duration::from_secs(2));
        let _ = swaymsg(&["output", &self.output, "unplug"]);
    }

    /// Stop everything and return the video-only file.
    pub fn stop(mut self) -> PathBuf {
        self.teardown();
        self.video_path.clone()
    }
}

impl Drop for WindowRecorder {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Add the mixed audio to the video by stream copy.
pub fn mux(video: &Path, audio: &Path, out: &Path) -> Result<()> {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video)
        .arg("-i")
        .arg(audio)
        .args(["-c", "copy", "-shortest"])
        .arg(out)
        .stdin(Stdio::null())
        .output()
        .context("ffmpeg not found")?;
    if !status.status.success() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&status.stderr).trim()));
    }
    Ok(())
}
