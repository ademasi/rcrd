use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

pub fn default_output_name() -> PathBuf {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = now.as_secs() as i64;
    let tm = time::OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or_else(|_| time::OffsetDateTime::UNIX_EPOCH);
    let datetime = format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.year(),
        tm.month() as u8,
        tm.day(),
        tm.hour(),
        tm.minute(),
        tm.second()
    );
    PathBuf::from(format!("rcrd-call-{datetime}.ogg"))
}

pub fn git_revision() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rev = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rev.is_empty() { None } else { Some(rev) }
}
