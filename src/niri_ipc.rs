use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

pub struct Size {
    pub width: i32,
    pub height: i32,
}

pub struct NiriOutput {
    pub name: String,
    pub logical: Size,
}

#[allow(dead_code)]
pub struct NiriWindow {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub size: Size,
}

fn niri_command() -> Command {
    Command::new(niri_bin())
}

pub fn niri_bin() -> String {
    if let Ok(path) = std::env::var("NIRI_BIN") {
        return path;
    }
    const CANDIDATES: &[&str] = &[
        "/run/current-system/sw/bin/niri",
        "/usr/bin/niri",
    ];
    for candidate in CANDIDATES {
        if Path::new(candidate).is_file() {
            return candidate.to_string();
        }
    }
    "niri".to_string()
}

pub fn list_outputs() -> anyhow::Result<Vec<NiriOutput>> {
    let out = niri_command()
        .args(["msg", "--json", "outputs"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg outputs failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    parse_outputs(&raw)
}

pub fn list_windows() -> anyhow::Result<Vec<NiriWindow>> {
    let out = niri_command()
        .args(["msg", "--json", "windows"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg windows failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    parse_windows(&raw)
}

pub fn focused_output_name() -> anyhow::Result<String> {
    let out = niri_command()
        .args(["msg", "--json", "focused-output"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg focused-output failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    #[derive(Deserialize)]
    struct OutputRaw {
        name: Option<String>,
    }
    let output: OutputRaw = serde_json::from_str(&raw)?;
    output.name.ok_or_else(|| anyhow::anyhow!("no name field in output"))
}

fn parse_outputs(raw: &str) -> anyhow::Result<Vec<NiriOutput>> {
    #[derive(Deserialize)]
    struct OutputRaw {
        name: Option<String>,
        logical: Option<SizeRaw>,
    }
    #[derive(Deserialize)]
    struct SizeRaw {
        width: i32,
        height: i32,
    }
    let map: HashMap<String, OutputRaw> = serde_json::from_str(raw)?;
    let outputs = map
        .into_iter()
        .filter_map(|(key, out)| {
            let logical = out.logical?;
            Some(NiriOutput {
                name: out.name.unwrap_or(key),
                logical: Size { width: logical.width, height: logical.height },
            })
        })
        .collect();
    Ok(outputs)
}

fn parse_windows(raw: &str) -> anyhow::Result<Vec<NiriWindow>> {
    #[derive(Deserialize)]
    struct WindowRaw {
        id: u64,
        title: Option<String>,
        app_id: Option<String>,
        layout: Option<LayoutRaw>,
    }
    #[derive(Deserialize)]
    struct LayoutRaw {
        window_size: Option<Vec<i32>>,
    }
    let windows: Vec<WindowRaw> = serde_json::from_str(raw)?;
    let windows = windows
        .into_iter()
        .filter_map(|w| {
            let layout = w.layout?;
            let size = layout.window_size?;
            Some(NiriWindow {
                id: w.id,
                title: w.title.unwrap_or_default(),
                app_id: w.app_id.unwrap_or_default(),
                size: Size { width: *size.first()?, height: *size.get(1)? },
            })
        })
        .collect();
    Ok(windows)
}

#[allow(dead_code)]
pub fn running_on_niri() -> bool {
    niri_command()
        .args(["msg", "--json", "focused-output"])
        .output()
        .ok()
        .and_then(|o| if o.status.success() { Some(()) } else { None })
        .is_some()
}
