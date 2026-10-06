use std::path::PathBuf;

pub fn default_output_name(prefix: &str, name: Option<&str>) -> PathBuf {
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
    let suffix = name
        .map(sanitize_name)
        .filter(|s| !s.is_empty())
        .map(|s| format!("-{s}"))
        .unwrap_or_default();
    PathBuf::from(format!("{prefix}{datetime}{suffix}.ogg"))
}

/// Make a user-provided label safe for filenames: whitespace becomes '-',
/// anything that isn't alphanumeric/'-'/'_' is dropped, runs of '-' collapse.
fn sanitize_name(name: &str) -> String {
    let mapped: String = name
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let mut out = String::with_capacity(mapped.len());
    for c in mapped.chars() {
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    out.trim_matches('-').to_string()
}
