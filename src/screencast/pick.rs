use crate::niri_ipc::NiriOutput;

pub fn show_picker(outputs: &[NiriOutput]) -> Option<String> {
    if outputs.is_empty() {
        return None;
    }
    if outputs.len() == 1 {
        return Some(outputs[0].name.clone());
    }

    let mut cmd = std::process::Command::new("zenity");
    cmd.arg("--list");
    cmd.arg("--title=Screen Sharing");
    cmd.arg("--text=Select which monitor to share:");
    cmd.arg("--column=Monitor");
    cmd.arg("--column=Resolution");
    for o in outputs {
        cmd.arg(&o.name);
        cmd.arg(format!("{}x{}", o.logical.width, o.logical.height));
    }
    cmd.arg("--width=400");
    cmd.arg("--height=300");

    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
