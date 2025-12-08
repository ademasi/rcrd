use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde_json::Value;
use std::os::unix::process::ExitStatusExt;

/// Record a call (Teams, Zoom, etc.) by tapping the current PipeWire sink monitor and microphone.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Output file path (default: rcrd-call-YYYYmmdd-HHMMSS.ogg)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Stop after this many seconds (omit to record until Ctrl+C)
    #[arg(short, long)]
    duration: Option<u32>,

    /// PipeWire sink node name to tap (monitor side). Defaults to current default sink.
    #[arg(long)]
    sink: Option<String>,

    /// PipeWire source node name to tap (microphone). Defaults to current default source.
    #[arg(long)]
    source: Option<String>,

    /// Do not record microphone; capture only the remote/output side.
    #[arg(long, default_value_t = false)]
    no_mic: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let defaults = detect_defaults().unwrap_or_default();

    let sink = args
        .sink
        .or(defaults.sink)
        .ok_or_else(|| anyhow!("Could not detect default sink; pass --sink <node.name>"))?;

    let source = if args.no_mic {
        None
    } else {
        Some(
            args.source
                .or(defaults.source)
                .ok_or_else(|| anyhow!("Could not detect default source; pass --source <node.name>"))?,
        )
    };

    let monitor = format!("{sink}.monitor");
    let outfile = args.output.unwrap_or_else(default_output_name);

    println!("Recording");
    println!("  output   : {}", outfile.display());
    println!("  monitor  : {monitor}");
    if let Some(src) = &source {
        println!("  mic      : {src}");
    } else {
        println!("  mic      : (disabled)");
    }
    if let Some(dur) = args.duration {
        println!("  duration : {dur}s");
    }
    let started_wall = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let started_instant = Instant::now();
    println!("  started  : {}", chrono_time(started_wall));
    println!("Press Ctrl+C to stop. Showing elapsed time…");

    let running = Arc::new(AtomicBool::new(true));
    let ticker = start_elapsed_ticker(started_instant, running.clone());
    run_ffmpeg(&monitor, source.as_deref(), &outfile, args.duration)?;
    running.store(false, Ordering::Relaxed);
    let _ = ticker.join();
    let elapsed = started_instant.elapsed();
    println!(
        "Finished. Elapsed: {:02}:{:02}:{:02}",
        elapsed.as_secs() / 3600,
        (elapsed.as_secs() / 60) % 60,
        elapsed.as_secs() % 60
    );
    Ok(())
}

#[derive(Default)]
struct Defaults {
    sink: Option<String>,
    source: Option<String>,
}

fn detect_defaults() -> Result<Defaults> {
    let output = Command::new("pw-dump")
        .output()
        .context("pw-dump failed (is pipewire-utils installed?)")?;
    if !output.status.success() {
        bail!("pw-dump exited with {}", output.status);
    }
    let root: Value = serde_json::from_slice(&output.stdout).context("pw-dump returned invalid JSON")?;
    let mut defaults = Defaults::default();
    let Some(array) = root.as_array() else { return Ok(defaults); };

    for obj in array {
        let Some(obj_type) = obj.get("type").and_then(Value::as_str) else { continue; };
        if obj_type != "PipeWire:Interface:Metadata" {
            continue;
        }
        let items = obj
            .get("metadata")
            .and_then(Value::as_array)
            .or_else(|| obj.get("info").and_then(|info| info.get("items")).and_then(Value::as_array));
        let Some(items) = items else { continue; };

        for item in items {
            let Some(key) = item.get("key").and_then(Value::as_str) else { continue; };
            match key {
                "default.audio.sink" | "default.configured.audio.sink" => {
                    if defaults.sink.is_none() {
                        defaults.sink = extract_name(item.get("value"));
                    }
                }
                "default.audio.source" | "default.configured.audio.source" => {
                    if defaults.source.is_none() {
                        defaults.source = extract_name(item.get("value"));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(defaults)
}

fn extract_name(val: Option<&Value>) -> Option<String> {
    let Some(val) = val else { return None; };
    if let Some(s) = val.as_str() {
        return Some(s.to_owned());
    }
    if let Some(obj) = val.as_object() {
        if let Some(name) = obj.get("name").and_then(Value::as_str) {
            return Some(name.to_owned());
        }
        if let Some(value) = obj.get("value").and_then(Value::as_str) {
            return Some(value.to_owned());
        }
    }
    None
}

fn chrono_time(now: Duration) -> String {
    use std::fmt::Write;
    // format YYYYmmdd-HHMMSS without pulling in chrono/time crates
    let secs = now.as_secs() as i64;
    let tm = time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or_else(|_| time::OffsetDateTime::UNIX_EPOCH);
    let mut out = String::new();
    let _ = write!(
        out,
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.year(),
        tm.month() as u8,
        tm.day(),
        tm.hour(),
        tm.minute(),
        tm.second()
    );
    out
}

fn default_output_name() -> PathBuf {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let datetime = chrono_time(now);
    PathBuf::from(format!("rcrd-call-{datetime}.ogg"))
}

fn start_elapsed_ticker(started: Instant, running: Arc<AtomicBool>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while running.load(Ordering::Relaxed) {
            let elapsed = started.elapsed();
            let h = elapsed.as_secs() / 3600;
            let m = (elapsed.as_secs() / 60) % 60;
            let s = elapsed.as_secs() % 60;
            print!("\rElapsed: {:02}:{:02}:{:02}", h, m, s);
            let _ = io::stdout().flush();
            thread::sleep(Duration::from_secs(1));
        }
        println!();
    })
}

fn run_ffmpeg(monitor: &str, mic: Option<&str>, outfile: &PathBuf, duration: Option<u32>) -> Result<()> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostdin"]);
    if let Some(d) = duration {
        cmd.args(["-t", &d.to_string()]);
    }
    cmd.args(["-nostats", "-loglevel", "warning"]);
    cmd.args(["-f", "pulse", "-i", monitor]);
    if let Some(mic_name) = mic {
        cmd.args(["-f", "pulse", "-i", mic_name]);
        cmd.args([
            "-filter_complex",
            "[0:a][1:a]amix=inputs=2:duration=longest:dropout_transition=3",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-c:a",
            "libopus",
            "-b:a",
            "128k",
        ]);
    } else {
        cmd.args([
            "-map",
            "0:a",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-c:a",
            "libopus",
            "-b:a",
            "128k",
        ]);
    }
    cmd.arg(outfile);

    let status = cmd.status().context("failed to spawn ffmpeg")?;
    if status.success() {
        return Ok(());
    }
    if let Some(sig) = status.signal() {
        // Treat SIGINT (Ctrl+C) as a clean shutdown.
        if sig == 2 {
            println!();
            return Ok(());
        }
        bail!("ffmpeg terminated by signal {sig}");
    }
    bail!("ffmpeg exited with {}", status);
}
