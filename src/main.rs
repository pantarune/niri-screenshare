mod niri_ipc;
mod screencast;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!("starting niri-screenshare");

    let conn = zbus::connection::Builder::session()?
        .name("org.freedesktop.impl.portal.desktop.niri")?
        .build()
        .await?;

    let screencast = screencast::ScreenCastInterface::new(conn.clone());

    conn.object_server()
        .at("/org/freedesktop/portal/desktop", screencast)
        .await?;

    tracing::info!("listening on org.freedesktop.impl.portal.desktop.niri");

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(86400)).await;
    }
}
