use crate::niri_ipc::{NiriOutput, NiriWindow};

pub enum PickerChoice {
    Monitor(String),
    Window(u64),
}

pub fn show_picker(
    outputs: &[NiriOutput],
    windows: &[NiriWindow],
) -> Option<PickerChoice> {
    let monitor_count = outputs.len();
    let window_count = windows.len();
    if monitor_count + window_count == 0 {
        return None;
    }
    if monitor_count + window_count == 1 {
        if let Some(o) = outputs.first() {
            return Some(PickerChoice::Monitor(o.name.clone()));
        }
        if let Some(w) = windows.first() {
            return Some(PickerChoice::Window(w.id));
        }
    }

    let mut cmd = std::process::Command::new("zenity");
    cmd.arg("--list");
    cmd.arg("--title=Screen Sharing");
    cmd.arg("--text=Select what to share:");
    cmd.arg("--column=ID");
    cmd.arg("--column=Type");
    cmd.arg("--column=Name");
    cmd.arg("--column=Size");
    cmd.arg("--print-column=1");

    for o in outputs {
        cmd.arg(format!("M:{}", o.name));
        cmd.arg("Monitor");
        cmd.arg(&o.name);
        cmd.arg(format!("{}x{}", o.logical.width, o.logical.height));
    }
    for w in windows {
        let label = if w.title.is_empty() { &w.app_id } else { &w.title };
        cmd.arg(format!("W:{}", w.id));
        cmd.arg("Window");
        cmd.arg(label);
        cmd.arg(format!("{}x{}", w.size.width, w.size.height));
    }

    cmd.arg("--width=600");
    cmd.arg("--height=400");

    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    if let Some(name) = s.strip_prefix("M:") {
        return Some(PickerChoice::Monitor(name.to_string()));
    }
    if let Some(id_str) = s.strip_prefix("W:") {
        if let Ok(id) = id_str.parse::<u64>() {
            return Some(PickerChoice::Window(id));
        }
    }
    None
}
