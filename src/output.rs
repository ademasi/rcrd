use std::path::PathBuf;
use std::process::Command;

pub fn default_output_name(prefix: &str) -> PathBuf {
    let tm = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let datetime = format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.year(),
        tm.month() as u8,
        tm.day(),
        tm.hour(),
        tm.minute(),
        tm.second()
    );
    PathBuf::from(format!("{prefix}{datetime}.ogg"))
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
