use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Deserialize)]
pub struct LogicalGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
}

#[derive(Debug, Deserialize)]
pub struct NiriOutput {
    pub name: String,
    pub logical: LogicalGeometry,
}

#[derive(Debug, Deserialize)]
pub struct NiriWindow {
    pub id: u64,
    pub title: String,
    #[serde(rename = "app-id", default)]
    pub app_id: String,
    #[serde(default)]
    pub is_focused: bool,
}

pub fn list_outputs() -> anyhow::Result<Vec<NiriOutput>> {
    let out = Command::new("niri")
        .args(["msg", "--json", "outputs"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg outputs failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    let map: HashMap<String, NiriOutput> = serde_json::from_str(&raw)?;
    let mut outputs: Vec<NiriOutput> = map.into_values().collect();
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(outputs)
}

pub fn focused_output_name() -> anyhow::Result<String> {
    let out = Command::new("niri")
        .args(["msg", "--json", "focused-output"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg focused-output failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    let output: NiriOutput = serde_json::from_str(&raw)?;
    Ok(output.name)
}
