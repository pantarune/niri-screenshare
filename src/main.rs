mod niri_ipc;
mod portal_config;
mod screencast;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .init();

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--picker") | Some("--debug-picker") => {
            #[cfg(feature = "picker")]
            {
                // No tokio: GTK needs the main thread. Choice on stdout, logs on stderr.
                match screencast::run_picker_process() {
                    Some(screencast::DebugPickerChoice::Monitor(name)) => {
                        println!("M:{name}");
                    }
                    Some(screencast::DebugPickerChoice::Window(id)) => {
                        println!("W:{id}");
                    }
                    None => {
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            #[cfg(not(feature = "picker"))]
            {
                anyhow::bail!("--picker requires building with --features picker");
            }
        }
        Some(arg) => {
            anyhow::bail!(
                "unknown argument: {arg}\nusage: niri-screenshare [--picker|--debug-picker]"
            );
        }
        None => run_portal(),
    }
}

#[tokio::main]
async fn run_portal() -> anyhow::Result<()> {
    tracing::info!("starting niri-screenshare");
    portal_config::ensure_portals_config();

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
