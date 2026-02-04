use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::util::{push_log, transcription as trans_const};

#[derive(Clone, Debug, Default)]
pub struct TransSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// Supervises a `whisper-stream` process that is started/stopped via `active`.
pub fn start_transcriber(
    model_path: PathBuf,
    binary_path: PathBuf,
    pulse_source: String,
    language: Arc<Mutex<String>>,
    transcript: Arc<Mutex<Vec<TransSegment>>>,
    recent_logs: Arc<Mutex<Vec<String>>>,
    active: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    base_offset_ms: Arc<AtomicI64>,
    reset: Arc<AtomicBool>,
    threads: usize,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut restart_delay = Duration::from_millis(200);

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            if !active.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
                continue;
            }

            let lang = language
                .lock()
                .map(|l| l.clone())
                .unwrap_or_else(|_| "en".to_string());

            let base = base_offset_ms.load(Ordering::Relaxed);

            let mut cmd = Command::new(&binary_path);
            cmd.args([
                "-m",
                model_path.to_string_lossy().as_ref(),
                "-t",
                &threads.to_string(),
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
            cmd.env("PULSE_SOURCE", &pulse_source);
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());

            match cmd.spawn() {
                Ok(mut child) => {
                    push_log(&recent_logs, "whisper-stream started");
                    let start = Instant::now();

                    // forward stderr to logs
                    if let Some(stderr) = child.stderr.take() {
                        let logs = recent_logs.clone();
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
                            if stop.load(Ordering::Relaxed)
                                || !active.load(Ordering::Relaxed)
                            {
                                break;
                            }

                            if reset.swap(false, Ordering::Relaxed) {
                                if let Ok(mut t) = transcript.lock() {
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

                            let ts = base
                                + start.elapsed().as_millis() as i64;

                            if let Ok(mut t) = transcript.lock() {
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
                    push_log(&recent_logs, "whisper-stream stopped");
                }
                Err(e) => {
                    push_log(
                        &recent_logs,
                        format!(
                            "failed to start whisper-stream: {}",
                            e
                        ),
                    );
                    active.store(false, Ordering::Relaxed);
                    thread::sleep(restart_delay);
                    continue;
                }
            }

            // If still active, loop to maybe restart; otherwise wait for next toggle
            thread::sleep(restart_delay);
        }
    })
}
