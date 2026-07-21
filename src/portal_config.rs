use std::io::Write;
use std::path::Path;

const NEEDED_KEY: &str = "org.freedesktop.impl.portal.ScreenCast";
const NEEDED_VALUE: &str = "niri";

pub fn ensure_portals_config() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let path = Path::new(&home)
        .join(".config")
        .join("xdg-desktop-portal")
        .join("portals.conf");

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
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

fn patch_existing(path: &Path) -> std::io::Result<bool> {
    let content = std::fs::read_to_string(path)?;

    if content.contains(&format!("{NEEDED_KEY}={NEEDED_VALUE}")) {
        return Ok(false);
    }

    let entry = format!("{NEEDED_KEY}={NEEDED_VALUE}");
    let mut out = String::new();
    let mut inserted = false;
    let mut in_preferred = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if in_preferred && !inserted
            && trimmed.starts_with('[')
            && trimmed != "[preferred]"
        {
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

    let tmp = path.with_extension("conf.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(out.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;

    Ok(true)
}

fn write_new(path: &Path) -> std::io::Result<()> {
    let content = format!(
        "\
[preferred]
default=gtk
{NEEDED_KEY}={NEEDED_VALUE}
"
    );
    std::fs::write(path, content)
}
