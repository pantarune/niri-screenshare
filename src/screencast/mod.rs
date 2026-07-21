mod pw_backend;

use std::collections::HashMap;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::fdo;
use zbus::interface;
use zbus::zvariant::{Array, Fd as ZvariantFd, ObjectPath, OwnedValue, Signature, Value};
use zbus::Connection;

use crate::niri_ipc;

const MUTTER_SCREENCAST_DEST: &str = "org.gnome.Mutter.ScreenCast";
const MUTTER_SCREENCAST_PATH: &str = "/org/gnome/Mutter/ScreenCast";

pub struct ScreenCastInterface {
    state: Arc<Mutex<HashMap<String, CaptureSession>>>,
    conn: Option<Connection>,
}

struct CaptureSession {
    app_id: String,
    started: bool,
    niri_session_path: Option<String>,
    niri_stream_path: Option<String>,
    cursor_mode: u32,
    output_name: Option<String>,
    node_id: u32,
}

struct SessionHandler {
    state: Arc<Mutex<HashMap<String, CaptureSession>>>,
    session_id: String,
    conn: Option<Connection>,
}

impl ScreenCastInterface {
    pub fn new(conn: Connection) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            conn: Some(conn),
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionHandler {
    async fn close(&mut self) -> fdo::Result<()> {
        tracing::info!("Session.Close: session={}", self.session_id);
        let mut state = self.state.lock().await;
        if let Some(session) = state.remove(&self.session_id) {
            Self::stop_niri(&self.conn, &session.niri_session_path).await;
        }
        Ok(())
    }
}

impl SessionHandler {
    async fn stop_niri(conn: &Option<Connection>, session_path: &Option<String>) {
        if let (Some(ref conn), Some(ref p)) = (conn, session_path) {
            let _ = conn.call_method(
                Some(MUTTER_SCREENCAST_DEST), p.as_str(),
                Some("org.gnome.Mutter.ScreenCast.Session"), "Stop", &(),
            ).await;
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastInterface {
    #[zbus(property, name = "version")]
    fn version_prop(&self) -> u32 { 5 }
    #[zbus(property)]
    fn AvailableSourceTypes(&self) -> u32 { 1 }
    #[zbus(property)]
    fn AvailableCursorModes(&self) -> u32 { 7 }

    async fn create_session(
        &mut self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        tracing::info!("CreateSession: session={}", session_handle);
        let sh = session_handle.to_string();
        self.state.lock().await.insert(sh.clone(), CaptureSession {
            app_id: app_id.to_string(),
            started: false,
            niri_session_path: None,
            niri_stream_path: None,

            cursor_mode: 1,
            output_name: None,
            node_id: 0,
        });
        if let Some(ref conn) = self.conn {
            if let Ok(p) = ObjectPath::try_from(sh.as_str()) {
                let _ = conn.object_server().at(p, SessionHandler {
                    state: self.state.clone(),
                    session_id: sh.clone(),
                    conn: self.conn.clone(),
                }).await;
            }
        }
        Ok((0, HashMap::new()))
    }

    async fn select_sources(
        &mut self,
        _request_handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        _app_id: &str,
        options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        tracing::info!("SelectSources: session={}", session_handle);

        fn val_u32(v: &OwnedValue) -> Option<u32> {
            use zbus::zvariant::Value;
            let val: &Value<'_> = v;
            match val {
                Value::U32(u) => Some(*u),
                _ => None,
            }
        }
        let types = options.get("types").and_then(val_u32).unwrap_or(1);
        let cursor = options.get("cursor_mode").and_then(val_u32).unwrap_or(1);

        let mut state = self.state.lock().await;
        let session = state.get_mut(session_handle.as_str()).ok_or_else(|| {
            fdo::Error::Failed(format!("session {} not found", session_handle))
        })?;

        session.cursor_mode = cursor;
        session.output_name = niri_ipc::focused_output_name().ok()
            .or_else(|| niri_ipc::list_outputs().ok()?.into_iter().next().map(|o| o.name));

        tracing::info!("types={} cursor={} output={:?}", types, cursor, session.output_name);

        let mut results = HashMap::new();
        results.insert("available_source_types".into(), OwnedValue::from(1u32));
        results.insert("available_cursor_modes".into(), OwnedValue::from(7u32));
        Ok((0, results))
    }

    async fn start(
        &mut self,
        _request_handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        tracing::info!("Start: session={}", session_handle);
        let conn = self.conn.clone().ok_or_else(|| fdo::Error::Failed("no D-Bus connection".into()))?;

        let (output_name, cursor_mode) = {
            let mut s = self.state.lock().await;
            let session = s.get_mut(session_handle.as_str()).ok_or_else(|| {
                fdo::Error::Failed(format!("session {} not found", session_handle))
            })?;
            if session.started {
                return Err(fdo::Error::Failed("session already started".into()));
            }
            session.started = true;
            (session.output_name.clone(), session.cursor_mode)
        };

        let name = output_name.as_deref().unwrap_or("HDMI-A-1");
        let (width, height) = get_output_size(name).unwrap_or((1920, 1080));

        let niri_session = create_niri_session(&conn).await
            .map_err(|e| fdo::Error::Failed(format!("create session: {e}")))?;
        let niri_stream = record_niri_monitor(&conn, &niri_session, name, cursor_mode).await
            .map_err(|e| fdo::Error::Failed(format!("record: {e}")))?;
        let node_id = start_and_get_node_id(&conn, &niri_session, &niri_stream).await
            .map_err(|e| fdo::Error::Failed(format!("start: {e}")))?;

        {
            let mut s = self.state.lock().await;
            if let Some(session) = s.get_mut(session_handle.as_str()) {
                session.niri_session_path = Some(niri_session);
                session.niri_stream_path = Some(niri_stream);
                session.node_id = node_id;
            }
        }

        tracing::info!("node={} size={}x{}", node_id, width, height);

        let mut results = HashMap::new();
        let mut sp: HashMap<String, Value<'_>> = HashMap::new();
        sp.insert("source_type".into(), Value::from(1u32));
        sp.insert("size".into(), Value::from((width as i32, height as i32)));
        let stream_val: Value<'_> = (node_id, sp).into();
        let mut arr = Array::new(&Signature::from_bytes(b"(ua{sv})").unwrap());
        arr.append(stream_val).unwrap();
        results.insert("streams".into(), Value::Array(arr).try_to_owned().unwrap());
        Ok((0, results))
    }

    #[zbus(name = "OpenPipeWireRemote")]
    async fn open_pipewire_remote(
        &mut self,
        _session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<ZvariantFd<'static>> {
        let raw = pw_backend::open_fd()
            .map_err(|e| fdo::Error::Failed(format!("open pipewire socket: {e}")))?;
        Ok(ZvariantFd::Owned(unsafe { OwnedFd::from_raw_fd(raw) }))
    }

    async fn close(
        &mut self,
        session_handle: ObjectPath<'_>,
        _app_id: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<()> {
        tracing::info!("Close: session={}", session_handle);
        let mut state = self.state.lock().await;
        if let Some(session) = state.remove(session_handle.as_str()) {
            SessionHandler::stop_niri(&self.conn, &session.niri_session_path).await;
        }
        Ok(())
    }
}

async fn create_niri_session(conn: &Connection) -> anyhow::Result<String> {
    let msg = conn.call_method(Some(MUTTER_SCREENCAST_DEST), MUTTER_SCREENCAST_PATH,
        Some("org.gnome.Mutter.ScreenCast"), "CreateSession",
        &HashMap::<&str, OwnedValue>::new(),
    ).await?;
    let body = msg.body();
    let path: ObjectPath<'_> = body.deserialize()?;
    let result = path.as_str().to_string();
    drop(body);
    Ok(result)
}

async fn record_niri_monitor(
    conn: &Connection, session_path: &str, monitor: &str,
    cursor_mode: u32,
) -> anyhow::Result<String> {
    let mut opts: HashMap<&str, OwnedValue> = HashMap::new();
    opts.insert("cursor_mode", OwnedValue::from(cursor_mode));
    let msg = conn.call_method(Some(MUTTER_SCREENCAST_DEST), session_path,
        Some("org.gnome.Mutter.ScreenCast.Session"), "RecordMonitor",
        &(monitor, opts),
    ).await?;
    let body = msg.body();
    let path: ObjectPath<'_> = body.deserialize()?;
    let result = path.as_str().to_string();
    drop(body);
    Ok(result)
}

async fn start_and_get_node_id(conn: &Connection, session_path: &str, stream_path: &str) -> anyhow::Result<u32> {
    use zbus::proxy::Proxy;

    let proxy = Proxy::new(conn, MUTTER_SCREENCAST_DEST, stream_path,
        "org.gnome.Mutter.ScreenCast.Stream",
    ).await?;

    let mut signal = proxy.receive_signal("PipeWireStreamAdded").await?;

    conn.call_method(Some(MUTTER_SCREENCAST_DEST), session_path,
        Some("org.gnome.Mutter.ScreenCast.Session"), "Start", &(),
    ).await?;

    use tokio::time::timeout;
    use futures_util::StreamExt;

    match timeout(std::time::Duration::from_secs(10), signal.next()).await {
        Ok(Some(msg)) => {
            let (node_id,): (u32,) = msg.body().deserialize()?;
            Ok(node_id)
        }
        Ok(None) => anyhow::bail!("signal stream ended"),
        Err(_) => anyhow::bail!("timeout waiting for pipewire node"),
    }
}

fn get_output_size(output_name: &str) -> Option<(u32, u32)> {
    if let Ok(outputs) = niri_ipc::list_outputs() {
        for out in &outputs {
            if out.name == output_name || output_name.is_empty() {
                return Some((out.logical.width as u32, out.logical.height as u32));
            }
        }
        if let Some(first) = outputs.first() {
            return Some((first.logical.width as u32, first.logical.height as u32));
        }
    }
    None
}
