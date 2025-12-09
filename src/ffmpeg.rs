use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use regex::Regex;

pub fn prepare_mic_control() -> Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("rcrd-mic");
    fs::create_dir_all(&dir)?;
    let cmd_path = dir.join(format!("mic-{}.cmd", std::process::id()));

    // Initialize with unmute command
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&cmd_path)?;
    writeln!(f, "0.0 volume@micvol volume 1.0")?;

    Ok(cmd_path)
}

pub fn write_mic_volume(cmd_path: &Path, volume: f32) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(cmd_path)?;
    writeln!(f, "0.0 volume@micvol volume {volume}")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_ffmpeg(
    monitor: &str,
    mic: Option<&str>,
    mic_cmd_path: Option<&Path>,
    outfile: &Path,
    duration: Option<u32>,
    audio_level: Arc<Mutex<f32>>,
    recent_logs: Arc<Mutex<Vec<String>>>,
    debug: bool,
) -> Result<Child> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostdin", "-y"]);
    if let Some(d) = duration {
        cmd.args(["-t", &d.to_string()]);
    }

    cmd.args(["-f", "pulse", "-i", monitor]);

    let mut filter_complex = String::new();

    if let Some(mic_name) = mic {
        cmd.args(["-f", "pulse", "-i", mic_name]);
        let mic_cmd = if let Some(cmd_path) = mic_cmd_path {
            format!("filename='{}'", cmd_path.display())
        } else {
            String::from("filename=''")
        };

        filter_complex.push_str(&format!(
            "[1:a]asendcmd={mic_cmd},volume@micvol=volume=1.0[mic];\
             [0:a][mic]amix=inputs=2:duration=longest:dropout_transition=3[mix];\
             [mix]split[out_file][analysis];\
             [analysis]astats=metadata=1:reset=1,anullsink"
        ));

        cmd.args(["-filter_complex", &filter_complex]);
        cmd.args(["-map", "[out_file]"]);
    } else {
        filter_complex.push_str(
            "[0:a]split[out_file][analysis];[analysis]astats=metadata=1:reset=1,anullsink",
        );
        cmd.args(["-filter_complex", &filter_complex]);
        cmd.args(["-map", "[out_file]"]);
    }

    cmd.args([
        "-ac", "2", "-ar", "48000", "-c:a", "libopus", "-b:a", "128k",
    ]);
    cmd.arg(outfile);

    if debug {
        println!("FFmpeg command: {:?}", cmd);
        return Ok(cmd.spawn().context("failed to spawn ffmpeg")?);
    }

    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().context("failed to spawn ffmpeg")?;

    let stderr = child.stderr.take().expect("failed to capture stderr");

    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let re = Regex::new(r"RMS level dB:\s+([-0-9.]+)").unwrap();

        for line in reader.lines() {
            if let Ok(l) = line {
                if let Ok(mut logs) = recent_logs.lock() {
                    if logs.len() >= 10 {
                        logs.remove(0);
                    }
                    logs.push(l.clone());
                }

                if let Some(caps) = re.captures(&l) {
                    if let Some(m) = caps.get(1) {
                        if let Ok(val) = m.as_str().parse::<f32>() {
                            if let Ok(mut lock) = audio_level.lock() {
                                *lock = val;
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(child)
}
