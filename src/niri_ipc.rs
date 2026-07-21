use std::process::Command;

pub struct Size {
    pub width: i32,
    pub height: i32,
}

pub struct NiriOutput {
    pub name: String,
    pub logical: Size,
}

pub struct NiriWindow {
    pub id: u64,
    pub app_id: String,
    pub title: String,
    pub window_size: Size,
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
    parse_outputs(&raw)
}

pub fn focused_output_name() -> anyhow::Result<String> {
    let out = Command::new("niri")
        .args(["msg", "--json", "focused-output"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg focused-output failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let raw = String::from_utf8(out.stdout)?;
    // Extract the "name" field from the JSON object
    extract_json_string(&raw, "name").ok_or_else(|| anyhow::anyhow!("no name field in output"))
}

pub fn focused_window() -> anyhow::Result<NiriWindow> {
    let out = Command::new("niri")
        .args(["msg", "--json", "focused-window"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("niri msg focused-window failed");
    }
    let raw = String::from_utf8(out.stdout)?;

    // Debug: try each extraction
    parse_window(&raw).ok_or_else(|| anyhow::anyhow!("failed to parse focused window from niri"))
}

fn parse_window(raw: &str) -> Option<NiriWindow> {
    let id = extract_json_u64(raw, "id")?;
    let app_id = extract_json_string(raw, "app_id").unwrap_or_default();
    let title = extract_json_string(raw, "title").unwrap_or_default();
    let is_focused = extract_json_bool(raw, "is_focused").unwrap_or(false);
    let layout_str = extract_json_object(raw, "layout")?;
    // window_size might be an array [w, h] or not present
    let (width, height) = if let Some(ws) = extract_json_array(layout_str, "window_size") {
        (extract_array_int(ws, 0)?, extract_array_int(ws, 1)?)
    } else if let Some(ws) = extract_json_object(layout_str, "window_size") {
        let w = extract_json_int(ws, "width")?;
        let h = extract_json_int(ws, "height")?;
        (w, h)
    } else {
        return None;
    };
    Some(NiriWindow {
        id,
        app_id,
        title,
        window_size: Size { width, height },
        is_focused,
    })
}

fn extract_json_u64(raw: &str, key: &str) -> Option<u64> {
    let search = format!("\"{}\"", key);
    let idx = raw.find(&search)?;
    let after_key = &raw[idx + search.len()..];
    let colon = after_key.find(':')?;
    let num_str = after_key[colon + 1..].trim();
    let end = num_str.find(|c: char| !c.is_digit(10)).unwrap_or(num_str.len());
    num_str[..end].parse().ok()
}

fn extract_json_bool(raw: &str, key: &str) -> Option<bool> {
    let search = format!("\"{}\"", key);
    let idx = raw.find(&search)?;
    let after_key = &raw[idx + search.len()..];
    let colon = after_key.find(':')?;
    let val = after_key[colon + 1..].trim();
    if val.starts_with("true") { Some(true) }
    else if val.starts_with("false") { Some(false) }
    else { None }
}

fn parse_outputs(raw: &str) -> anyhow::Result<Vec<NiriOutput>> {
    let mut outputs = Vec::new();
    let mut rest = raw.trim();

    // Expect the opening `{`
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
        // Extract key (output name)
        let colon = rest.find(':').unwrap_or(0);
        let key = rest[..colon].trim();
        let key = key.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
            .unwrap_or("").to_string();
        rest = &rest[colon + 1..];

        // Find the matching closing brace for the value object
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

fn extract_array_int(raw: &str, index: usize) -> Option<i32> {
    let mut depth = 0;
    let mut idx = 0;
    let mut i = 0;
    while i < raw.len() {
        let c = raw.as_bytes()[i] as char;
        if c == '[' {
            depth += 1;
            if depth == 1 && idx == index {
                // value starts after '['
                let start = i + 1;
                let end = raw[start..].find(|c: char| c == ',' || c == ']')?;
                return raw[start..start + end].trim().parse().ok();
            }
        } else if c == ']' {
            if depth == 1 { break; }
            depth -= 1;
        } else if c == ',' && depth == 1 {
            idx += 1;
            if idx == index {
                let start = i + 1;
                let end = raw[start..].find(|c: char| c == ',' || c == ']')?;
                return raw[start..start + end].trim().parse().ok();
            }
        }
        i += 1;
    }
    None
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
