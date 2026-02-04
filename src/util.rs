use std::sync::{Arc, Mutex};

pub const LOG_BUFFER_SIZE: usize = 10;

pub mod audio {
    pub const SAMPLE_RATE: u32 = 48000;
    pub const CHANNELS: u8 = 2;
    pub const BITRATE: &str = "128k";
}

pub mod transcription {
    pub const DEFAULT_THREADS: usize = 8;
    pub const VOICE_THRESHOLD: &str = "0.75";
    pub const SEGMENT_LENGTH_MS: &str = "30000";
    pub const SEGMENT_STEP: &str = "0";
}

pub mod ui {
    pub const POLL_INTERVAL_MS: u64 = 50;
}

pub fn push_log(logs: &Arc<Mutex<Vec<String>>>, msg: impl Into<String>) {
    if let Ok(mut l) = logs.lock() {
        if l.len() >= LOG_BUFFER_SIZE {
            l.remove(0);
        }
        l.push(msg.into());
    }
}

pub fn format_timecode(ms: i64) -> String {
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1000) % 60;
    let millis = ms % 1000;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, millis)
}
