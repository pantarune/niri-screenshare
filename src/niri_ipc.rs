use std::process::Command;

pub struct Size {
    pub width: i32,
    pub height: i32,
}

pub struct NiriOutput {
    pub name: String,
    pub logical: Size,
}

pub fn list_outputs() -> anyhow::Result<Vec<NiriOutput>> {
    let out = Command::new("niri")
        .args(["msg", "--json", "outputs"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg outputs failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    parse_outputs(&raw)
}

pub struct NiriWindow {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub size: Size,
}

pub fn list_windows() -> anyhow::Result<Vec<NiriWindow>> {
    let out = Command::new("niri")
        .args(["msg", "--json", "windows"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg windows failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    parse_windows(&raw)
}

pub fn focused_output_name() -> anyhow::Result<String> {
    let out = Command::new("niri")
        .args(["msg", "--json", "focused-output"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg focused-output failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    extract_json_string(&raw, "name").ok_or_else(|| anyhow::anyhow!("no name field in output"))
}

fn parse_outputs(raw: &str) -> anyhow::Result<Vec<NiriOutput>> {
    let mut outputs = Vec::new();
    let mut rest = raw.trim();

    if !rest.starts_with('{') {
        anyhow::bail!("expected '{{' at start of outputs");
    }
    rest = &rest[1..];

    while let Some(end) = rest.find(|c| c == '}' || c == ',') {
        if rest[..end].trim().is_empty() {
            rest = &rest[end + 1..];
            continue;
        }
        if &rest[end..end+1] == "}" {
            break;
        }
        let colon = rest.find(':').unwrap_or(0);
        let key = rest[..colon].trim();
        let key = key.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
            .unwrap_or("").to_string();
        rest = &rest[colon + 1..];

        let mut depth = 0;
        let mut val_end = 0;
        for (i, c) in rest.char_indices() {
            if c == '{' { depth += 1; }
            else if c == '}' { depth -= 1; if depth == 0 { val_end = i + 1; break; } }
        }
        if val_end == 0 { break; }
        let val_str = &rest[..val_end];
        rest = &rest[val_end..];

        if let Some(out) = parse_output(val_str) {
            outputs.push(NiriOutput {
                name: if out.name.is_empty() { key.clone() } else { out.name },
                logical: out.logical,
            });
        }
    }

    Ok(outputs)
}

struct OutputRaw {
    name: String,
    logical: Size,
}

fn parse_output(raw: &str) -> Option<OutputRaw> {
    let name = extract_json_string(raw, "name").unwrap_or_default();
    let logical_str = extract_json_object(raw, "logical")?;
    let width = extract_json_int(&logical_str, "width")?;
    let height = extract_json_int(&logical_str, "height")?;
    Some(OutputRaw { name, logical: Size { width, height } })
}

fn extract_json_string(raw: &str, key: &str) -> Option<String> {
    let search = format!("\"{}\"", key);
    let idx = raw.find(&search)?;
    let after_key = &raw[idx + search.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim().strip_prefix('"')?;
    let end = after_colon.find('"')?;
    Some(after_colon[..end].to_string())
}

fn extract_json_int(raw: &str, key: &str) -> Option<i32> {
    let search = format!("\"{}\"", key);
    let idx = raw.find(&search)?;
    let after_key = &raw[idx + search.len()..];
    let colon = after_key.find(':')?;
    let num_str = after_key[colon + 1..].trim();
    let end = num_str.find(|c: char| !c.is_digit(10) && c != '-').unwrap_or(num_str.len());
    num_str[..end].parse().ok()
}

fn extract_json_uint(raw: &str, key: &str) -> Option<u64> {
    let search = format!("\"{}\"", key);
    let idx = raw.find(&search)?;
    let after_key = &raw[idx + search.len()..];
    let colon = after_key.find(':')?;
    let num_str = after_key[colon + 1..].trim();
    let end = num_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(num_str.len());
    num_str[..end].parse().ok()
}

fn extract_json_array<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let search = format!("\"{}\"", key);
    let idx = raw.find(&search)?;
    let after_key = &raw[idx + search.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim();
    if !after_colon.starts_with('[') { return None; }
    let mut depth = 0;
    let mut end = 0;
    for (i, c) in after_colon.char_indices() {
        if c == '[' { depth += 1; }
        else if c == ']' { depth -= 1; if depth == 0 { end = i + 1; break; } }
    }
    if end == 0 { None } else { Some(&after_colon[..end]) }
}

fn parse_json_int_at(arr: &str, index: usize) -> Option<i32> {
    let inner = arr.trim().trim_start_matches('[').trim_end_matches(']');
    inner.split(',')
        .nth(index)
        .and_then(|s| s.trim().parse().ok())
}

fn parse_windows(raw: &str) -> anyhow::Result<Vec<NiriWindow>> {
    let mut windows = Vec::new();
    let mut rest = raw.trim();

    if !rest.starts_with('[') {
        anyhow::bail!("expected '[' at start of windows");
    }
    rest = &rest[1..];

    loop {
        rest = rest.trim_start();
        if rest.is_empty() || rest.starts_with(']') {
            break;
        }
        if rest.starts_with(',') {
            rest = &rest[1..];
            continue;
        }
        if !rest.starts_with('{') {
            break;
        }

        let mut depth = 0u32;
        let mut end = 0;
        for (i, c) in rest.char_indices() {
            if c == '{' { depth += 1; }
            else if c == '}' { depth -= 1; if depth == 0 { end = i + 1; break; } }
        }
        if end == 0 { break; }

        let obj = &rest[..end];
        if let Some(win) = parse_window(obj) {
            windows.push(win);
        }
        rest = &rest[end..];
    }

    Ok(windows)
}

fn parse_window(raw: &str) -> Option<NiriWindow> {
    let id = extract_json_uint(raw, "id")?;
    let title = extract_json_string(raw, "title").unwrap_or_default();
    let app_id = extract_json_string(raw, "app_id").unwrap_or_default();
    let layout = extract_json_object(raw, "layout")?;
    let size_arr = extract_json_array(&layout, "window_size")?;
    let width = parse_json_int_at(&size_arr, 0)?;
    let height = parse_json_int_at(&size_arr, 1)?;
    Some(NiriWindow { id, title, app_id, size: Size { width, height } })
}

fn extract_json_object<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let search = format!("\"{}\"", key);
    let idx = raw.find(&search)?;
    let after_key = &raw[idx + search.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim();
    if !after_colon.starts_with('{') { return None; }
    let mut depth = 0;
    let mut end = 0;
    for (i, c) in after_colon.char_indices() {
        if c == '{' { depth += 1; }
        else if c == '}' { depth -= 1; if depth == 0 { end = i + 1; break; } }
    }
    if end == 0 { None } else { Some(&after_colon[..end]) }
}
