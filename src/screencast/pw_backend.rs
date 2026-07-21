use std::os::unix::io::{IntoRawFd, RawFd};

use anyhow::Context;

pub fn open_pipewire_fd(_session_handle: &str) -> anyhow::Result<RawFd> {
    let socket_path = pipewire_socket_path();
    let socket = std::os::unix::net::UnixStream::connect(&socket_path)
        .with_context(|| format!("connecting to {}", socket_path))?;
    Ok(socket.into_raw_fd())
}

pub fn open_pipewire_with_permissions(_node_id: u32) -> anyhow::Result<RawFd> {
    open_pipewire_fd("")
}

fn pipewire_socket_path() -> String {
    std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
        format!("{}/pipewire-0", runtime)
    })
}
