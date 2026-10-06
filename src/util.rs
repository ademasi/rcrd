use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

pub const LOG_BUFFER_SIZE: usize = 50;

pub mod audio {
    pub const SAMPLE_RATE: u32 = 48000;
    pub const CHANNELS: u8 = 2;
    pub const BITRATE: &str = "128k";
}

pub mod ui {
    /// Key poll interval while recording.
    pub const POLL_INTERVAL_MS: u64 = 100;
    /// How often the live status line is redrawn.
    pub const REDRAW_MS: u64 = 250;
    /// How often the system default devices are re-read in follow mode.
    pub const DEFAULTS_POLL_MS: u64 = 2000;
}

pub mod video {
    /// Refresh rate of the headless output the window is mirrored onto.
    pub const FPS: u32 = 30;
    pub const MAX_WIDTH: u32 = 3840;
    pub const MAX_HEIGHT: u32 = 2160;
}

/// `51:11`, `1:02:05` (guideline human time).
pub fn human_time(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

pub fn push_log(logs: &Arc<Mutex<Vec<String>>>, msg: impl Into<String>) {
    if let Ok(mut l) = logs.lock() {
        if l.len() >= LOG_BUFFER_SIZE {
            l.remove(0);
        }
        l.push(msg.into());
    }
}

pub fn last_logs(logs: &Arc<Mutex<Vec<String>>>, n: usize) -> Vec<String> {
    logs.lock()
        .map(|l| l.iter().rev().take(n).rev().cloned().collect())
        .unwrap_or_default()
}

/// Stop a child gracefully: send `sig` (so it can finalize its output file),
/// wait up to `grace`, then SIGKILL.
pub fn stop_child(child: &mut Child, sig: Signal, grace: Duration) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    let _ = kill(Pid::from_raw(child.id() as i32), sig);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}
