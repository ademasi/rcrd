use std::process::Command;
use std::sync::{Arc, Mutex};

use crate::util::push_log;

/// Holds the temporary PulseAudio modules we load to create a mixed monitor+mic source.
pub struct TranscriptionMix {
    pub source: String,
    modules: Vec<u32>,
}

impl Drop for TranscriptionMix {
    fn drop(&mut self) {
        for id in self.modules.drain(..) {
            let _ = Command::new("pactl")
                .arg("unload-module")
                .arg(id.to_string())
                .output();
        }
    }
}

/// Create a temporary null sink and loopback monitor + mic into it. Returns None if mic is None.
pub fn setup_mix_source(
    monitor: &str,
    mic: Option<&str>,
    logs: Arc<Mutex<Vec<String>>>,
) -> Option<TranscriptionMix> {
    let mic = mic?;
    let pid = std::process::id();
    let sink_name = format!("rcrd_mix_{}", pid);
    let mut modules = Vec::new();

    // Create null sink
    match Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            &format!("sink_name={}", sink_name),
            "sink_properties=device.description=\"rcrd mix\"",
        ])
        .output()
    {
        Ok(out) if out.status.success() => {
            if let Ok(id) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>() {
                modules.push(id);
                push_log(
                    &logs,
                    format!("Created mix sink {sink_name} (module {id}) for transcription"),
                );
            } else {
                push_log(&logs, "Failed to parse module-null-sink id; transcription may be monitor-only");
                return None;
            }
        }
        Ok(out) => {
            push_log(
                &logs,
                format!(
                    "Failed to create mix sink: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            );
            return None;
        }
        Err(e) => {
            push_log(&logs, format!("pactl load-module failed: {e}"));
            return None;
        }
    }

    // Loopback monitor -> sink
    match Command::new("pactl")
        .args([
            "load-module",
            "module-loopback",
            &format!("source={}", monitor),
            &format!("sink={}", sink_name),
            "latency_msec=1",
        ])
        .output()
    {
        Ok(out) if out.status.success() => {
            if let Ok(id) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>() {
                modules.push(id);
            }
        }
        Ok(out) => {
            push_log(
                &logs,
                format!(
                    "Failed to loopback monitor: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            );
        }
        Err(e) => {
            push_log(&logs, format!("pactl loopback monitor failed: {e}"));
        }
    }

    // Loopback mic -> sink
    match Command::new("pactl")
        .args([
            "load-module",
            "module-loopback",
            &format!("source={}", mic),
            &format!("sink={}", sink_name),
            "latency_msec=1",
        ])
        .output()
    {
        Ok(out) if out.status.success() => {
            if let Ok(id) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>() {
                modules.push(id);
            }
        }
        Ok(out) => {
            push_log(
                &logs,
                format!(
                    "Failed to loopback mic: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            );
        }
        Err(e) => {
            push_log(&logs, format!("pactl loopback mic failed: {e}"));
        }
    }

    Some(TranscriptionMix {
        source: format!("{sink_name}.monitor"),
        modules,
    })
}
