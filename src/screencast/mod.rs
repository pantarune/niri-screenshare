#[cfg(feature = "picker")]
mod pick;
mod pw_backend;

#[cfg(feature = "picker")]
pub use pick::{
    run_picker_process, show_picker as debug_show_picker, PickerChoice as DebugPickerChoice,
};

use std::collections::HashMap;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use zbus::fdo;
use zbus::interface;
use zbus::zvariant::{Array, Fd as ZvariantFd, ObjectPath, OwnedValue, Signature, Value};
use zbus::Connection;

use crate::niri_ipc;

const MUTTER_SCREENCAST_DEST: &str = "org.gnome.Mutter.ScreenCast";
const MUTTER_SCREENCAST_PATH: &str = "/org/gnome/Mutter/ScreenCast";

/// How long to reuse a picker choice across client session retries.
#[cfg(feature = "picker")]
const PICKER_REUSE_TTL: Duration = Duration::from_secs(10);

pub struct ScreenCastInterface {
    state: Arc<Mutex<HashMap<String, CaptureSession>>>,
    conn: Option<Connection>,
    #[cfg(feature = "picker")]
    picker: Arc<PickerCoordinator>,
}

struct CaptureSession {
    started: bool,
    niri_session_path: Option<String>,
    niri_stream_path: Option<String>,
    cursor_mode: u32,
    source_type: u32,
    output_name: Option<String>,
    window_id: Option<u64>,
    node_id: u32,
}

struct SessionHandler {
    state: Arc<Mutex<HashMap<String, CaptureSession>>>,
    session_id: String,
    conn: Option<Connection>,
    #[cfg(feature = "picker")]
    picker: Arc<PickerCoordinator>,
}

/// In-flight picker child plus a short-lived selection cache for retrying clients.
#[cfg(feature = "picker")]
struct PickerCoordinator {
    child: pick::PickerChildSlot,
    picking_session: std::sync::Mutex<Option<String>>,
    recent: std::sync::Mutex<Option<RecentPickerChoice>>,
}

#[cfg(feature = "picker")]
struct RecentPickerChoice {
    at: Instant,
    app_id: String,
    choice: pick::PickerChoice,
}

#[cfg(feature = "picker")]
impl PickerCoordinator {
    fn new() -> Self {
        Self {
            child: Arc::new(std::sync::Mutex::new(None)),
            picking_session: std::sync::Mutex::new(None),
            recent: std::sync::Mutex::new(None),
        }
    }

    fn take_reusable(&self, app_id: &str) -> Option<pick::PickerChoice> {
        let guard = self.recent.lock().ok()?;
        let recent = guard.as_ref()?;
        if recent.at.elapsed() > PICKER_REUSE_TTL {
            return None;
        }
        // Empty app_id on either side still matches.
        if !recent.app_id.is_empty()
            && !app_id.is_empty()
            && recent.app_id != app_id
        {
            return None;
        }
        tracing::info!(
            "reusing picker choice from {}s ago (app_id={:?})",
            recent.at.elapsed().as_secs(),
            app_id
        );
        Some(recent.choice.clone())
    }

    fn remember(&self, app_id: &str, choice: pick::PickerChoice) {
        if let Ok(mut guard) = self.recent.lock() {
            *guard = Some(RecentPickerChoice {
                at: Instant::now(),
                app_id: app_id.to_string(),
                choice,
            });
        }
    }

    fn clear_recent(&self) {
        if let Ok(mut guard) = self.recent.lock() {
            *guard = None;
        }
    }

    fn begin_picking(&self, session_id: &str) {
        if let Ok(mut guard) = self.picking_session.lock() {
            *guard = Some(session_id.to_string());
        }
    }

    fn end_picking(&self, session_id: &str) {
        if let Ok(mut guard) = self.picking_session.lock() {
            if guard.as_deref() == Some(session_id) {
                *guard = None;
            }
        }
    }

    fn cancel_if_picking(&self, session_id: &str) {
        let ours = self
            .picking_session
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .is_some_and(|s| s == session_id);
        if ours {
            pick::kill_slotted_picker(&self.child);
            self.end_picking(session_id);
        }
    }
}

impl ScreenCastInterface {
    pub fn new(conn: Connection) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            conn: Some(conn),
            #[cfg(feature = "picker")]
            picker: Arc::new(PickerCoordinator::new()),
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionHandler {
    async fn close(&mut self) -> fdo::Result<()> {
        tracing::info!("Session.Close: session={}", self.session_id);
        #[cfg(feature = "picker")]
        self.picker.cancel_if_picking(&self.session_id);
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
    fn available_source_types(&self) -> u32 { 3 }
    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        // Hidden | Embedded. Skip Metadata, Electron often blacks out with it.
        1 | 2
    }

    async fn create_session(
        &mut self,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        _app_id: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        tracing::info!("CreateSession: session={}", session_handle);
        let sh = session_handle.to_string();
        self.state.lock().await.insert(sh.clone(), CaptureSession {
            started: false,
            niri_session_path: None,
            niri_stream_path: None,

            cursor_mode: 1,
            source_type: 1,
            output_name: None,
            window_id: None,
            node_id: 0,
        });
        if let Some(ref conn) = self.conn {
            if let Ok(p) = ObjectPath::try_from(sh.as_str()) {
                let _ = conn.object_server().at(p, SessionHandler {
                    state: self.state.clone(),
                    session_id: sh.clone(),
                    conn: self.conn.clone(),
                    #[cfg(feature = "picker")]
                    picker: self.picker.clone(),
                }).await;
            }
        }
        Ok((0, HashMap::new()))
    }

    async fn select_sources(
        &mut self,
        _request_handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: &str,
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

        // portal: 1=Hidden 2=Embedded 4=Metadata; niri: 0=Hidden 1=Embedded 2=Metadata
        let cursor_portal = options.get("cursor_mode").and_then(val_u32);
        // Prefer Embedded. Metadata often yields a black frame in Electron.
        let cursor_niri = match cursor_portal {
            Some(1) => 0,
            _ => 1,
        };

        let mut state = self.state.lock().await;
        let session = state.get_mut(session_handle.as_str()).ok_or_else(|| {
            fdo::Error::Failed(format!("session {} not found", session_handle))
        })?;

        session.cursor_mode = cursor_niri;
        session.source_type = options.get("source_type").and_then(val_u32).unwrap_or(1);

        #[cfg(feature = "picker")]
        if std::env::var("NIRI_SCREENSHARE_PICKER").is_ok() {
            let session_id = session_handle.to_string();
            let app_id = app_id.to_string();

            // Reuse a recent choice so retrying clients only see one dialog.
            let reused = self.picker.take_reusable(&app_id);
            if let Some(choice) = reused {
                apply_picker_choice(session, choice);
                drop(state);
                return Ok((0, select_sources_results()));
            }

            drop(state);

            let outputs = match niri_ipc::list_outputs() {
                Ok(o) => o,
                Err(e) => {
                    tracing::error!("list_outputs failed: {e}");
                    Vec::new()
                }
            };
            let windows = match niri_ipc::list_windows() {
                Ok(w) => w,
                Err(e) => {
                    tracing::error!("list_windows failed: {e}");
                    Vec::new()
                }
            };
            tracing::info!(
                "picker targets: {} display(s), {} window(s)",
                outputs.len(),
                windows.len()
            );

            self.picker.begin_picking(&session_id);
            let child_slot = self.picker.child.clone();
            let choice = tokio::task::spawn_blocking(move || {
                pick::show_picker_cancellable(&outputs, &windows, Some(child_slot))
            })
            .await
            .map_err(|e| fdo::Error::Failed(format!("picker task failed: {e}")))?;
            self.picker.end_picking(&session_id);

            let mut state = self.state.lock().await;
            let session = state.get_mut(session_handle.as_str()).ok_or_else(|| {
                fdo::Error::Failed(format!("session {} not found", session_handle))
            })?;

            match choice {
                Some(choice) => {
                    self.picker.remember(&app_id, choice.clone());
                    apply_picker_choice(session, choice);
                }
                None => {
                    // response 1 = user cancelled
                    self.picker.clear_recent();
                    tracing::info!("SelectSources: user cancelled");
                    return Ok((1, HashMap::new()));
                }
            }

            tracing::info!("cursor={} source={} output={:?} window={:?}",
                cursor_niri, session.source_type, session.output_name, session.window_id);

            return Ok((0, select_sources_results()));
        }

        session.output_name = niri_ipc::focused_output_name().ok()
            .or_else(|| niri_ipc::list_outputs().ok()?.into_iter().next().map(|o| o.name));

        tracing::info!("cursor={} source={} output={:?} window={:?}",
            cursor_niri, session.source_type, session.output_name, session.window_id);

        Ok((0, select_sources_results()))
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

        let (source_type, output_name, window_id, cursor_mode) = {
            let mut s = self.state.lock().await;
            let session = s.get_mut(session_handle.as_str()).ok_or_else(|| {
                fdo::Error::Failed(format!("session {} not found", session_handle))
            })?;
            if session.started {
                return Err(fdo::Error::Failed("session already started".into()));
            }
            session.started = true;
            (session.source_type, session.output_name.clone(), session.window_id, session.cursor_mode)
        };

        let niri_session = create_niri_session(&conn).await
            .map_err(|e| fdo::Error::Failed(format!("create session: {e}")))?;

        let (niri_stream, width, height) = if source_type & 2 != 0 {
            let wid = window_id.ok_or_else(|| fdo::Error::Failed("no window id".into()))?;
            let stream = record_niri_window(&conn, &niri_session, wid, cursor_mode).await
                .map_err(|e| fdo::Error::Failed(format!("record window: {e}")))?;
            let size = get_window_size(wid).unwrap_or((960, 1048));
            (stream, size.0, size.1)
        } else {
            let name = output_name.as_deref()
                .ok_or_else(|| fdo::Error::Failed("no output selected".into()))?;
            let stream = record_niri_monitor(&conn, &niri_session, name, cursor_mode).await
                .map_err(|e| fdo::Error::Failed(format!("record monitor: {e}")))?;
            let size = get_output_size(name).unwrap_or((1920, 1080));
            (stream, size.0, size.1)
        };

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

        tracing::info!("node={} size={}x{} type={}", node_id, width, height, source_type);

        let mut results = HashMap::new();
        let mut sp: HashMap<String, Value<'_>> = HashMap::new();
        sp.insert("source_type".into(), Value::from(if source_type & 2 != 0 { 2u32 } else { 1u32 }));
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
        session_handle: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
    ) -> fdo::Result<ZvariantFd<'static>> {
        tracing::info!("OpenPipeWireRemote: session={}", session_handle);
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

fn select_sources_results() -> HashMap<String, OwnedValue> {
    let mut results = HashMap::new();
    results.insert("available_source_types".into(), OwnedValue::from(3u32));
    results.insert("available_cursor_modes".into(), OwnedValue::from(3u32));
    results
}

#[cfg(feature = "picker")]
fn apply_picker_choice(session: &mut CaptureSession, choice: pick::PickerChoice) {
    match choice {
        pick::PickerChoice::Monitor(name) => {
            session.source_type = 1;
            session.output_name = Some(name);
            session.window_id = None;
        }
        pick::PickerChoice::Window(id) => {
            session.source_type = 2;
            session.output_name = None;
            session.window_id = Some(id);
        }
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
    opts.insert("cursor-mode", OwnedValue::from(cursor_mode));
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

async fn record_niri_window(
    conn: &Connection, session_path: &str, window_id: u64,
    cursor_mode: u32,
) -> anyhow::Result<String> {
    let mut opts: HashMap<&str, OwnedValue> = HashMap::new();
    opts.insert("cursor-mode", OwnedValue::from(cursor_mode));
    opts.insert("window-id", OwnedValue::from(window_id));
    let msg = conn.call_method(Some(MUTTER_SCREENCAST_DEST), session_path,
        Some("org.gnome.Mutter.ScreenCast.Session"), "RecordWindow",
        &opts,
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

fn get_window_size(window_id: u64) -> Option<(u32, u32)> {
    if let Ok(windows) = niri_ipc::list_windows() {
        for w in &windows {
            if w.id == window_id {
                return Some((w.size.width as u32, w.size.height as u32));
            }
        }
    }
    None
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
