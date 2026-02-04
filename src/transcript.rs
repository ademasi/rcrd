use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use crate::state::SharedState;
use crate::util::{push_log, transcription as trans_const};

#[derive(Clone, Debug, Default)]
pub struct TransSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

pub struct TranscriberConfig {
    pub model_path: PathBuf,
    pub binary_path: PathBuf,
    pub pulse_source: String,
    pub threads: usize,
}

/// Supervises a `whisper-stream` process that is started/stopped via `active`.
pub fn start_transcriber(config: TranscriberConfig, shared: SharedState) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let restart_delay = Duration::from_millis(200);

        loop {
            if shared.transcription_stop.load(Ordering::Relaxed) {
                break;
            }

            if !shared.transcription_active.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            let lang = shared
                .language
                .lock()
                .map(|l| l.clone())
                .unwrap_or_else(|_| "en".to_string());

            let base = shared.base_offset_ms.load(Ordering::Relaxed);

            let mut cmd = Command::new(&config.binary_path);
            cmd.args([
                "-m",
                config.model_path.to_string_lossy().as_ref(),
                "-t",
                &config.threads.to_string(),
                "-vth",
                trans_const::VOICE_THRESHOLD,
                "--length",
                trans_const::SEGMENT_LENGTH_MS,
                "--step",
                trans_const::SEGMENT_STEP,
                "-l",
                &lang,
            ]);
            // Force whisper-stream to listen to the call monitor source
            cmd.env("SDL_AUDIODRIVER", "pulse");
            cmd.env("PULSE_SOURCE", &config.pulse_source);
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());

            match cmd.spawn() {
                Ok(mut child) => {
                    push_log(&shared.logs, "whisper-stream started");
                    let start = Instant::now();

                    // forward stderr to logs
                    if let Some(stderr) = child.stderr.take() {
                        let logs = shared.logs.clone();
                        thread::spawn(move || {
                            let reader = BufReader::new(stderr);
                            for line in reader.lines() {
                                if let Ok(l) = line {
                                    push_log(&logs, format!("[whisper] {l}"));
                                }
                            }
                        });
                    }

                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines() {
                            if shared.transcription_stop.load(Ordering::Relaxed)
                                || !shared.transcription_active.load(Ordering::Relaxed)
                            {
                                break;
                            }

                            if shared.transcription_reset.swap(false, Ordering::Relaxed) {
                                if let Ok(mut t) = shared.transcript.lock() {
                                    t.clear();
                                }
                            }

                            let text = match line {
                                Ok(t) => t.trim().to_string(),
                                Err(_) => break,
                            };
                            if text.is_empty() {
                                continue;
                            }

                            let ts = base + start.elapsed().as_millis() as i64;

                            if let Ok(mut t) = shared.transcript.lock() {
                                t.push(TransSegment {
                                    start_ms: ts,
                                    end_ms: ts,
                                    text,
                                });
                            }
                        }
                    }

                    let _ = child.kill();
                    let _ = child.wait();
                    push_log(&shared.logs, "whisper-stream stopped");
                }
                Err(e) => {
                    push_log(
                        &shared.logs,
                        format!("failed to start whisper-stream: {}", e),
                    );
                    shared.transcription_active.store(false, Ordering::Relaxed);
                    thread::sleep(restart_delay);
                    continue;
                }
            }

            // If still active, loop to maybe restart; otherwise wait for next toggle
            thread::sleep(restart_delay);
        }
    })
}
