use std::os::unix::io::{IntoRawFd, RawFd};

use anyhow::Context;

pub fn open_fd() -> anyhow::Result<RawFd> {
    let path = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| {
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
        format!("{dir}/pipewire-0")
    });
    let socket = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connect to {path}"))?;
    Ok(socket.into_raw_fd())
}
