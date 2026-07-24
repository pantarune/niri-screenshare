use std::os::unix::io::{IntoRawFd, RawFd};

use anyhow::Context;

fn runtime_dir() -> String {
    std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| {
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "1000".into());
        format!("/run/user/{uid}")
    })
}

pub fn open_fd() -> anyhow::Result<RawFd> {
    let path = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| {
        format!("{}/pipewire-0", runtime_dir())
    });
    let socket = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connect to {path}"))?;
    Ok(socket.into_raw_fd())
}
