use std::sync::{Arc, Mutex};

pub const LOG_BUFFER_SIZE: usize = 10;

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
