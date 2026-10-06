use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::process::{Command, Stdio};

#[derive(Default, Clone)]
pub struct Defaults {
    pub sink: Option<String>,
    pub source: Option<String>,
}

impl Defaults {
    /// Fill missing entries from `fallback`.
    fn or(self, fallback: Defaults) -> Defaults {
        Defaults {
            sink: self.sink.or(fallback.sink),
            source: self.source.or(fallback.source),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AudioDevice {
    pub node_name: String,
    pub description: String,
}

impl std::fmt::Display for AudioDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.description.is_empty() || self.description == self.node_name {
            write!(f, "{}", self.node_name)
        } else {
            write!(f, "{} ({})", self.description, self.node_name)
        }
    }
}

pub struct DeviceInfo {
    pub defaults: Defaults,
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
}

/// Parse pw-dump once and return defaults, sinks, and sources.
pub fn enumerate_devices() -> Result<DeviceInfo> {
    let root = get_pw_dump()?;
    let array = root.as_array().ok_or_else(|| anyhow!("pw-dump returned non-array JSON"))?;

    let mut active = Defaults::default();
    let mut configured = Defaults::default();
    let mut sinks = Vec::new();
    let mut sources = Vec::new();

    for obj in array {
        let Some(obj_type) = obj.get("type").and_then(Value::as_str) else {
            continue;
        };

        match obj_type {
            "PipeWire:Interface:Metadata" => {
                parse_defaults(obj, &mut active, &mut configured);
            }
            "PipeWire:Interface:Node" => {
                if let Some(device) = parse_audio_node(obj) {
                    let class = obj
                        .get("info")
                        .and_then(|i| i.get("props"))
                        .and_then(|p| p.get("media.class"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    match class {
                        "Audio/Sink" => sinks.push(device),
                        "Audio/Source" => sources.push(device),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(DeviceInfo {
        // `default.audio.*` is the device currently in use. The
        // `default.configured.*` entry is only WirePlumber's remembered
        // preference and may name a device that is not even connected.
        defaults: active.or(configured),
        sinks,
        sources,
    })
}

fn get_pw_dump() -> Result<Value> {
    let output = Command::new("pw-dump")
        .output()
        .context(
            "pw-dump failed. Ensure pipewire-utils is installed.\n\
             Try: --sink <name> --source <name> to specify devices manually.\n\
             Run: pw-dump | grep node.name to list available devices.",
        )?;
    if !output.status.success() {
        return Err(anyhow!(
            "pw-dump exited with {}.\n\
             Try: --sink <name> --source <name> to specify devices manually.",
            output.status
        ));
    }
    serde_json::from_slice(&output.stdout).context("pw-dump returned invalid JSON")
}

fn parse_defaults(obj: &Value, active: &mut Defaults, configured: &mut Defaults) {
    let items = obj
        .get("metadata")
        .and_then(Value::as_array)
        .or_else(|| {
            obj.get("info")
                .and_then(|info| info.get("items"))
                .and_then(Value::as_array)
        });
    let Some(items) = items else { return };

    for item in items {
        let Some(key) = item.get("key").and_then(Value::as_str) else {
            continue;
        };
        let Some(name) = extract_name(item.get("value")) else {
            continue;
        };
        match key {
            "default.audio.sink" => active.sink = Some(name),
            "default.audio.source" => active.source = Some(name),
            "default.configured.audio.sink" => configured.sink = Some(name),
            "default.configured.audio.source" => configured.source = Some(name),
            _ => {}
        }
    }
}

fn parse_audio_node(obj: &Value) -> Option<AudioDevice> {
    let props = obj.get("info").and_then(|i| i.get("props"))?;
    let node_name = props
        .get("node.name")
        .and_then(Value::as_str)?
        .to_string();
    let description = props
        .get("node.description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(AudioDevice {
        node_name,
        description,
    })
}

fn extract_name(val: Option<&Value>) -> Option<String> {
    let val = val?;
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

pub fn find_device<'a>(list: &'a [AudioDevice], name: &str) -> Option<&'a AudioDevice> {
    list.iter().find(|d| d.node_name == name)
}

/// Just the description (falls back to the node name) - for compact UI lines.
pub fn short_describe(list: &[AudioDevice], name: &str) -> String {
    match find_device(list, name) {
        Some(d) if !d.description.is_empty() => d.description.clone(),
        Some(d) => d.node_name.clone(),
        None => name.to_string(),
    }
}

pub fn list_devices(list: &[AudioDevice]) -> String {
    list.iter()
        .map(|d| format!("  {d}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The currently active default sink/source, read via pw-metadata (much
/// cheaper than a full pw-dump, suitable for polling).
pub fn current_defaults() -> Result<Defaults> {
    Ok(Defaults {
        sink: read_default_metadata("default.audio.sink")?,
        source: read_default_metadata("default.audio.source")?,
    })
}

fn read_default_metadata(key: &str) -> Result<Option<String>> {
    let output = Command::new("pw-metadata")
        .args(["-n", "default", "0", key])
        .output()
        .context("pw-metadata failed. Ensure pipewire is installed.")?;
    // update: id:0 key:'default.audio.sink' value:'{"name":"..."}' type:'Spa:String:JSON'
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(start) = text.find("value:'") else {
        return Ok(None);
    };
    let rest = &text[start + "value:'".len()..];
    let Some(end) = rest.find('\'') else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(&rest[..end]).unwrap_or(Value::Null);
    Ok(extract_name(Some(&value)))
}

/// Find the audio capture stream node that process `pid` opened: the sink
/// monitor capture if `monitor` is true, otherwise the microphone capture.
pub fn find_capture_stream(pid: u32, monitor: bool) -> Result<Option<u32>> {
    let root = get_pw_dump()?;
    let Some(array) = root.as_array() else {
        return Ok(None);
    };
    for obj in array {
        if obj.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = obj.get("info").and_then(|i| i.get("props")) else {
            continue;
        };
        if props.get("media.class").and_then(Value::as_str) != Some("Stream/Input/Audio") {
            continue;
        }
        let stream_pid = props.get("application.process.id").and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });
        if stream_pid != Some(u64::from(pid)) {
            continue;
        }
        let captures_sink = props
            .get("stream.capture.sink")
            .is_some_and(|v| v.as_bool() == Some(true) || v.as_str() == Some("true"));
        if captures_sink == monitor {
            return Ok(obj.get("id").and_then(Value::as_u64).map(|id| id as u32));
        }
    }
    Ok(None)
}

/// Clear a stream's fixed target so WirePlumber moves it whenever the
/// default device changes (pipewire-pulse pins streams opened by device
/// name, even for @DEFAULT_MONITOR@).
pub fn unpin_stream(node_id: u32) -> Result<()> {
    let status = Command::new("pw-metadata")
        .args([&node_id.to_string(), "target.object", "-1", "Spa:Id"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("pw-metadata failed")?;
    if !status.success() {
        return Err(anyhow!("pw-metadata exited with {status}"));
    }
    Ok(())
}
