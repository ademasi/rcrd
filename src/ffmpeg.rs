use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};

use crate::util::{audio, push_log};

pub struct FfmpegConfig<'a> {
    pub monitor: &'a str,
    pub mic: Option<&'a str>,
    pub mic_cmd_path: Option<&'a Path>,
    pub outfile: &'a Path,
    pub duration: Option<u32>,
    pub debug: bool,
}

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

pub fn spawn_ffmpeg(config: FfmpegConfig<'_>, logs: Arc<Mutex<Vec<String>>>) -> Result<Child> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostdin", "-y"]);

    cmd.args(["-f", "pulse", "-i", config.monitor]);

    let filter_complex = if let Some(mic_name) = config.mic {
        cmd.args(["-f", "pulse", "-i", mic_name]);
        let mic_cmd = if let Some(cmd_path) = config.mic_cmd_path {
            format!("filename={}", cmd_path.display())
        } else {
            String::from("filename=")
        };

        format!(
            "[1:a]asendcmd={mic_cmd},volume@micvol=volume=1.0[mic];\
             [0:a][mic]amix=inputs=2:duration=longest:dropout_transition=3[out_file]"
        )
    } else {
        String::from("[0:a]anull[out_file]")
    };

    cmd.args(["-filter_complex", &filter_complex]);
    cmd.args(["-map", "[out_file]"]);

    cmd.args([
        "-ac",
        &audio::CHANNELS.to_string(),
        "-ar",
        &audio::SAMPLE_RATE.to_string(),
        "-c:a",
        "libopus",
        "-b:a",
        audio::BITRATE,
    ]);
    // -t must be an output option: as an input option it only limits the
    // monitor stream, and amix duration=longest then runs on mic input forever.
    if let Some(d) = config.duration {
        cmd.args(["-t", &d.to_string()]);
    }
    cmd.arg(config.outfile);

    if config.debug {
        println!("FFmpeg command: {:?}", cmd);
        return cmd.spawn().context("failed to spawn ffmpeg");
    }

    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().context("failed to spawn ffmpeg")?;

    let Some(stderr) = child.stderr.take() else {
        return Ok(child);
    };

    thread::spawn(move || {
        let reader = BufReader::new(stderr);

        for l in reader.lines().map_while(Result::ok) {
            push_log(&logs, l);
        }
    });

    Ok(child)
}
