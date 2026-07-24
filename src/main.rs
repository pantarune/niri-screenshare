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
        Some("check") | Some("--check") => {
            cmd_check()
        }
        Some("--picker") | Some("--debug-picker") => {
            #[cfg(feature = "picker")]
            {
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
                "unknown argument: {arg}\nusage: niri-screenshare [check|--picker|--debug-picker]"
            );
        }
        None => run_portal(),
    }
}

fn cmd_check() -> anyhow::Result<()> {
    println!("niri-screenshare check\n");

    // 1. niri IPC
    print!("niri IPC ... ");
    match niri_ipc::focused_output_name() {
        Ok(name) => println!("OK (focused output: {name})"),
        Err(e) => println!("FAIL: {e}"),
    }

    // 2 + 3: D-Bus names via dbus-send
    for (label, name) in [
        ("portal backend", "org.freedesktop.impl.portal.desktop.niri"),
        ("Mutter.ScreenCast", "org.gnome.Mutter.ScreenCast"),
    ] {
        print!("{label} ... ");
        let out = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.DBus",
                "--type=method_call",
                "--print-reply",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus.NameHasOwner",
                &format!("string:{}", name),
            ])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                if stdout.contains("boolean true") {
                    println!("OK");
                } else {
                    println!("WARN: not on bus");
                }
            }
            _ => println!("WARN: dbus-send failed"),
        }
    }

    // 4. Portals config
    print!("portals.conf ... ");
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    let conf_path = std::path::Path::new(&home)
        .join(".config")
        .join("xdg-desktop-portal")
        .join("portals.conf");
    if conf_path.exists() {
        let content = std::fs::read_to_string(&conf_path).unwrap_or_default();
        if content.contains("ScreenCast=niri") {
            println!("OK");
        } else {
            println!("WARN: exists but missing ScreenCast=niri");
        }
    } else {
        println!("WARN: not found (auto-created on service start)");
    }

    println!();
    Ok(())
}

#[tokio::main]
async fn run_portal() -> anyhow::Result<()> {
    tracing::info!("starting niri-screenshare");
    tracing::debug!("stale sessions from previous instance will be dropped");
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
