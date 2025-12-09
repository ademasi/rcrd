use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::process::Command;

#[derive(Default, Clone)]
pub struct Defaults {
    pub sink: Option<String>,
    pub source: Option<String>,
}

pub fn detect_defaults() -> Result<Defaults> {
    let output = Command::new("pw-dump")
        .output()
        .context("pw-dump failed (is pipewire-utils installed?)")?;
    if !output.status.success() {
        return Err(anyhow!("pw-dump exited with {}", output.status));
    }
    let root: Value =
        serde_json::from_slice(&output.stdout).context("pw-dump returned invalid JSON")?;
    let mut defaults = Defaults::default();
    let Some(array) = root.as_array() else {
        return Ok(defaults);
    };

    for obj in array {
        let Some(obj_type) = obj.get("type").and_then(Value::as_str) else {
            continue;
        };
        if obj_type != "PipeWire:Interface:Metadata" {
            continue;
        }
        let items = obj.get("metadata").and_then(Value::as_array).or_else(|| {
            obj.get("info")
                .and_then(|info| info.get("items"))
                .and_then(Value::as_array)
        });
        let Some(items) = items else {
            continue;
        };

        for item in items {
            let Some(key) = item.get("key").and_then(Value::as_str) else {
                continue;
            };
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
    let Some(val) = val else {
        return None;
    };
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
