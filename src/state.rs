use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct SharedState {
    pub logs: Arc<Mutex<Vec<String>>>,
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            logs: Arc::new(Mutex::new(Vec::new())),
        }
    }
}
