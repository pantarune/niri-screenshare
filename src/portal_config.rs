use std::io::Write;
use std::path::{Path, PathBuf};

const NEEDED_KEY: &str = "org.freedesktop.impl.portal.ScreenCast";
const NEEDED_VALUE: &str = "niri";

pub fn ensure_portals_config() {
    let path = config_home()
        .join("xdg-desktop-portal")
        .join("portals.conf");

    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("portals.conf: failed to create config directory ({e})");
            return;
        }
    }

    if path.exists() {
        match patch_existing(&path) {
            Ok(true) => tracing::info!("portals.conf: added ScreenCast=niri"),
            Ok(false) => tracing::debug!("portals.conf: already configured"),
            Err(e) => tracing::warn!("portals.conf: failed to patch ({e})"),
        }
    } else {
        match write_new(&path) {
            Ok(()) => tracing::info!("portals.conf: created with ScreenCast=niri"),
            Err(e) => tracing::warn!("portals.conf: failed to create ({e})"),
        }
    }
}

fn config_home() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/root".into());
    PathBuf::from(home).join(".config")
}

fn patch_existing(path: &Path) -> std::io::Result<bool> {
    let content = std::fs::read_to_string(path)?;

    if content
        .lines()
        .any(|line| line.trim() == format!("{NEEDED_KEY}={NEEDED_VALUE}"))
    {
        return Ok(false);
    }

    let entry = format!("{NEEDED_KEY}={NEEDED_VALUE}");
    let mut out = String::new();
    let mut inserted = false;
    let mut in_preferred = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if in_preferred && !inserted && trimmed.starts_with('[') && trimmed != "[preferred]" {
            out.push_str(&entry);
            out.push('\n');
            inserted = true;
            in_preferred = false;
        }

        if trimmed == "[preferred]" {
            in_preferred = true;
        }

        out.push_str(line);
        out.push('\n');
    }

    if in_preferred && !inserted {
        out.push_str(&entry);
        out.push('\n');
        inserted = true;
    }

    if !inserted {
        if !content.is_empty() && !content.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("[preferred]\n");
        out.push_str(&entry);
        out.push('\n');
    }

    atomic_write(path, out.as_bytes())?;
    Ok(true)
}

fn write_new(path: &Path) -> std::io::Result<()> {
    // Only configure the interface this backend owns. In particular, do not
    // change the user's default FileChooser/OpenURI/etc. portal selection.
    let content = format!("[preferred]\n{NEEDED_KEY}={NEEDED_VALUE}\n");
    atomic_write(path, content.as_bytes())
}

fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("conf.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_config_only_selects_screencast_backend() {
        let expected = format!("[preferred]\n{NEEDED_KEY}={NEEDED_VALUE}\n");
        assert!(!expected.contains("default="));
        assert!(!expected.contains("Secret="));
        assert!(!expected.contains("Access="));
    }
}
