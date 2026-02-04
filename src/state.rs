use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex};

use crate::transcript::TransSegment;

#[derive(Clone)]
pub struct SharedState {
    pub logs: Arc<Mutex<Vec<String>>>,
    pub transcript: Arc<Mutex<Vec<TransSegment>>>,
    pub transcription_active: Arc<AtomicBool>,
    pub transcription_stop: Arc<AtomicBool>,
    pub transcription_reset: Arc<AtomicBool>,
    pub base_offset_ms: Arc<AtomicI64>,
    pub language: Arc<Mutex<String>>,
}

impl SharedState {
    pub fn new(language: String) -> Self {
        Self {
            logs: Arc::new(Mutex::new(Vec::new())),
            transcript: Arc::new(Mutex::new(Vec::new())),
            transcription_active: Arc::new(AtomicBool::new(false)),
            transcription_stop: Arc::new(AtomicBool::new(false)),
            transcription_reset: Arc::new(AtomicBool::new(false)),
            base_offset_ms: Arc::new(AtomicI64::new(0)),
            language: Arc::new(Mutex::new(language)),
        }
    }
}
