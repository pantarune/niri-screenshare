#[cfg(feature = "picker")]
mod pick;

#[cfg(feature = "picker")]
pub use pick::{run_picker_process, PickerChoice as DebugPickerChoice};

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use zbus::fdo;
use zbus::interface;
use zbus::zvariant::{Array, ObjectPath, OwnedValue, Signature, Value};
use zbus::Connection;

use crate::niri_ipc;

const MUTTER_SCREENCAST_DEST: &str = "org.gnome.Mutter.ScreenCast";
const MUTTER_SCREENCAST_PATH: &str = "/org/gnome/Mutter/ScreenCast";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureState {
    Created,
    Starting,
    Started,
}

pub struct ScreenCastInterface {
    state: Arc<Mutex<HashMap<String, CaptureSession>>>,
    conn: Option<Connection>,
    #[cfg(feature = "picker")]
    picker: Arc<PickerCoordinator>,
}

struct CaptureSession {
    state: CaptureState,
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

/// Tracks picker child processes by portal session so unrelated capture requests
/// cannot reuse, replace, or cancel one another's consent UI.
#[cfg(feature = "picker")]
struct PickerCoordinator {
    children: std::sync::Mutex<HashMap<String, pick::PickerChildSlot>>,
}

#[cfg(feature = "picker")]
impl PickerCoordinator {
    fn new() -> Self {
        Self {
            children: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn begin(&self, session_id: &str) -> pick::PickerChildSlot {
        let slot = Arc::new(std::sync::Mutex::new(None));
        let old = self
            .children
            .lock()
            .ok()
            .and_then(|mut children| children.insert(session_id.to_string(), slot.clone()));
        if let Some(old) = old {
            pick::kill_slotted_picker(&old);
        }
        slot
    }

    fn finish(&self, session_id: &str, slot: &pick::PickerChildSlot) {
        if let Ok(mut children) = self.children.lock() {
            let is_current = children
                .get(session_id)
                .is_some_and(|current| Arc::ptr_eq(current, slot));
            if is_current {
                children.remove(session_id);
            }
        }
    }

    fn cancel(&self, session_id: &str) {
        let slot = self
            .children
            .lock()
            .ok()
            .and_then(|mut children| children.remove(session_id));
        if let Some(slot) = slot {
            pick::kill_slotted_picker(&slot);
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
        self.picker.cancel(&self.session_id);

        let session = self.state.lock().await.remove(&self.session_id);
        if let (Some(conn), Some(session)) = (&self.conn, session) {
            if let Some(path) = session.niri_session_path.as_deref() {
                stop_niri_session(conn, path).await;
            }
        }

        // Session objects are one-shot. Remove the D-Bus object after Close so
        // a long-running portal daemon does not accumulate stale object paths.
        if let Some(conn) = &self.conn {
            if let Err(e) = conn
                .object_server()
                .remove::<SessionHandler, _>(self.session_id.as_str())
                .await
            {
                tracing::debug!(
                    "failed to unregister session object {}: {e}",
                    self.session_id
                );
            }
        }

        Ok(())
    }
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastInterface {
    // Version 4 introduced persistence/restore_data. We intentionally advertise
    // v3 until those semantics are implemented rather than claiming support.
    #[zbus(property, name = "version")]
    fn version_prop(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        1 | 2 | 4
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

        self.state.lock().await.insert(
            sh.clone(),
            CaptureSession {
                state: CaptureState::Created,
                niri_session_path: None,
                niri_stream_path: None,
                // XDG ScreenCast defaults cursor_mode to Hidden.
                cursor_mode: 0,
                source_type: 1,
                output_name: None,
                window_id: None,
                node_id: 0,
            },
        );

        if let Some(ref conn) = self.conn {
            if let Ok(p) = ObjectPath::try_from(sh.as_str()) {
                match conn
                    .object_server()
                    .at(
                        p,
                        SessionHandler {
                            state: self.state.clone(),
                            session_id: sh.clone(),
                            conn: self.conn.clone(),
                            #[cfg(feature = "picker")]
                            picker: self.picker.clone(),
                        },
                    )
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!("session path already registered: {sh}"),
                    Err(e) => tracing::warn!("failed to register session handler: {e}"),
                }
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

        let requested_types = normalize_source_types(options.get("types").and_then(value_u32))
            .ok_or_else(|| fdo::Error::InvalidArgs("no supported source type requested".into()))?;
        let cursor_niri = portal_cursor_to_niri(options.get("cursor_mode").and_then(value_u32));

        {
            let mut state = self.state.lock().await;
            let session = state.get_mut(session_handle.as_str()).ok_or_else(|| {
                fdo::Error::Failed(format!("session {} not found", session_handle))
            })?;
            if session.state != CaptureState::Created {
                return Err(fdo::Error::Failed(
                    "cannot select sources after Start".into(),
                ));
            }
            session.cursor_mode = cursor_niri;
            session.source_type = requested_types;
            session.output_name = None;
            session.window_id = None;
        }

        #[cfg(feature = "picker")]
        if std::env::var("NIRI_SCREENSHARE_NO_PICKER").is_err() {
            let outputs = if requested_types & 1 != 0 {
                match niri_ipc::list_outputs() {
                    Ok(outputs) => outputs,
                    Err(e) => {
                        tracing::error!("list_outputs failed: {e}");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let windows = if requested_types & 2 != 0 {
                match niri_ipc::list_windows() {
                    Ok(windows) => windows,
                    Err(e) => {
                        tracing::error!("list_windows failed: {e}");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

            if outputs.is_empty() && windows.is_empty() {
                tracing::warn!("SelectSources: no requested capture targets available");
                return Ok((2, HashMap::new()));
            }

            tracing::info!(
                "picker targets: {} display(s), {} window(s)",
                outputs.len(),
                windows.len()
            );

            let session_id = session_handle.to_string();
            let child_slot = self.picker.begin(&session_id);
            let slot_for_task = child_slot.clone();
            let joined = tokio::task::spawn_blocking(move || {
                pick::show_picker_cancellable(&outputs, &windows, Some(slot_for_task))
            })
            .await;
            self.picker.finish(&session_id, &child_slot);

            let choice =
                joined.map_err(|e| fdo::Error::Failed(format!("picker task failed: {e}")))?;

            let mut state = self.state.lock().await;
            let Some(session) = state.get_mut(session_handle.as_str()) else {
                // Session.Close can legitimately race with the picker.
                return Ok((1, HashMap::new()));
            };

            match choice {
                Some(choice) => apply_picker_choice(session, choice),
                None => {
                    tracing::info!("SelectSources: user cancelled");
                    return Ok((1, HashMap::new()));
                }
            }

            tracing::info!(
                "cursor={} source={} output={:?} window={:?}",
                cursor_niri,
                session.source_type,
                session.output_name,
                session.window_id
            );

            return Ok((0, select_sources_results()));
        }

        // Pickerless builds are intentionally conservative: they can auto-pick
        // a monitor but will not silently choose an arbitrary window.
        if requested_types & 1 == 0 {
            tracing::warn!("pickerless mode cannot satisfy a window-only request");
            return Ok((2, HashMap::new()));
        }

        let output_name = niri_ipc::focused_output_name().ok().or_else(|| {
            niri_ipc::list_outputs()
                .ok()?
                .into_iter()
                .next()
                .map(|o| o.name)
        });
        let Some(output_name) = output_name else {
            return Ok((2, HashMap::new()));
        };

        let mut state = self.state.lock().await;
        let session = state
            .get_mut(session_handle.as_str())
            .ok_or_else(|| fdo::Error::Failed(format!("session {} not found", session_handle)))?;
        session.source_type = 1;
        session.output_name = Some(output_name);
        session.window_id = None;

        tracing::info!(
            "cursor={} source={} output={:?} window={:?}",
            cursor_niri,
            session.source_type,
            session.output_name,
            session.window_id
        );

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
        let conn = self
            .conn
            .clone()
            .ok_or_else(|| fdo::Error::Failed("no D-Bus connection".into()))?;

        let (source_type, output_name, window_id, cursor_mode) = {
            let mut sessions = self.state.lock().await;
            let session = sessions.get_mut(session_handle.as_str()).ok_or_else(|| {
                fdo::Error::Failed(format!("session {} not found", session_handle))
            })?;

            match session.state {
                CaptureState::Created => session.state = CaptureState::Starting,
                CaptureState::Starting => {
                    return Err(fdo::Error::Failed("session is already starting".into()))
                }
                CaptureState::Started => {
                    return Err(fdo::Error::Failed("session already started".into()))
                }
            }

            (
                session.source_type,
                session.output_name.clone(),
                session.window_id,
                session.cursor_mode,
            )
        };

        let attempt = start_capture(
            &conn,
            source_type,
            output_name.as_deref(),
            window_id,
            cursor_mode,
        )
        .await;

        let (niri_session, niri_stream, node_id, size) = match attempt {
            Ok(value) => value,
            Err(e) => {
                if let Some(session) = self.state.lock().await.get_mut(session_handle.as_str()) {
                    if session.state == CaptureState::Starting {
                        session.state = CaptureState::Created;
                    }
                }
                return Err(fdo::Error::Failed(format!("start: {e}")));
            }
        };

        let committed = {
            let mut sessions = self.state.lock().await;
            if let Some(session) = sessions.get_mut(session_handle.as_str()) {
                if session.state == CaptureState::Starting {
                    session.state = CaptureState::Started;
                    session.niri_session_path = Some(niri_session.clone());
                    session.niri_stream_path = Some(niri_stream);
                    session.node_id = node_id;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };

        if !committed {
            stop_niri_session(&conn, &niri_session).await;
            return Err(fdo::Error::Failed("session closed while starting".into()));
        }

        tracing::info!("node={} size={:?} type={}", node_id, size, source_type);

        let mut results = HashMap::new();
        let mut stream_properties: HashMap<String, Value<'_>> = HashMap::new();
        stream_properties.insert(
            "source_type".into(),
            Value::from(if source_type & 2 != 0 { 2u32 } else { 1u32 }),
        );
        if let Some((width, height)) = size {
            stream_properties.insert("size".into(), Value::from((width as i32, height as i32)));
        }

        let stream_value: Value<'_> = (node_id, stream_properties).into();
        let mut streams = Array::new(&Signature::from_bytes(b"(ua{sv})").unwrap());
        streams.append(stream_value).unwrap();
        results.insert(
            "streams".into(),
            Value::Array(streams).try_to_owned().unwrap(),
        );
        Ok((0, results))
    }
}

fn value_u32(v: &OwnedValue) -> Option<u32> {
    let value: &Value<'_> = v;
    match value {
        Value::U32(value) => Some(*value),
        _ => None,
    }
}

fn normalize_source_types(value: Option<u32>) -> Option<u32> {
    let supported = value.unwrap_or(1) & 3;
    (supported != 0).then_some(supported)
}

// Portal: 1=Hidden, 2=Embedded, 4=Metadata.
// Mutter/niri: 0=Hidden, 1=Embedded, 2=Metadata.
fn portal_cursor_to_niri(value: Option<u32>) -> u32 {
    match value {
        Some(2) => 1,
        Some(4) => 2,
        // Hidden is the protocol default, including for absent/invalid values.
        _ => 0,
    }
}

fn select_sources_results() -> HashMap<String, OwnedValue> {
    let mut results = HashMap::new();
    results.insert("available_source_types".into(), OwnedValue::from(3u32));
    results.insert("available_cursor_modes".into(), OwnedValue::from(7u32));
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

async fn start_capture(
    conn: &Connection,
    source_type: u32,
    output_name: Option<&str>,
    window_id: Option<u64>,
    cursor_mode: u32,
) -> anyhow::Result<(String, String, u32, Option<(u32, u32)>)> {
    let niri_session = create_niri_session(conn).await?;

    let attempt = async {
        let (stream, size) = if source_type & 2 != 0 {
            let window_id = window_id.ok_or_else(|| anyhow::anyhow!("no window selected"))?;
            let stream = record_niri_window(conn, &niri_session, window_id, cursor_mode).await?;
            (stream, get_window_size(window_id))
        } else {
            let output_name = output_name.ok_or_else(|| anyhow::anyhow!("no output selected"))?;
            let stream = record_niri_monitor(conn, &niri_session, output_name, cursor_mode).await?;
            (stream, get_output_size(output_name))
        };

        let node_id = start_and_get_node_id(conn, &niri_session, &stream).await?;
        Ok::<_, anyhow::Error>((stream, node_id, size))
    }
    .await;

    match attempt {
        Ok((stream, node_id, size)) => Ok((niri_session, stream, node_id, size)),
        Err(e) => {
            stop_niri_session(conn, &niri_session).await;
            Err(e)
        }
    }
}

async fn stop_niri_session(conn: &Connection, session_path: &str) {
    if let Err(e) = conn
        .call_method(
            Some(MUTTER_SCREENCAST_DEST),
            session_path,
            Some("org.gnome.Mutter.ScreenCast.Session"),
            "Stop",
            &(),
        )
        .await
    {
        tracing::debug!("failed to stop screencast session {session_path}: {e}");
    }
}

async fn create_niri_session(conn: &Connection) -> anyhow::Result<String> {
    let msg = conn
        .call_method(
            Some(MUTTER_SCREENCAST_DEST),
            MUTTER_SCREENCAST_PATH,
            Some("org.gnome.Mutter.ScreenCast"),
            "CreateSession",
            &HashMap::<&str, OwnedValue>::new(),
        )
        .await?;
    let body = msg.body();
    let path: ObjectPath<'_> = body.deserialize()?;
    Ok(path.as_str().to_string())
}

async fn record_niri_monitor(
    conn: &Connection,
    session_path: &str,
    monitor: &str,
    cursor_mode: u32,
) -> anyhow::Result<String> {
    let mut options: HashMap<&str, OwnedValue> = HashMap::new();
    options.insert("cursor-mode", OwnedValue::from(cursor_mode));
    let msg = conn
        .call_method(
            Some(MUTTER_SCREENCAST_DEST),
            session_path,
            Some("org.gnome.Mutter.ScreenCast.Session"),
            "RecordMonitor",
            &(monitor, options),
        )
        .await?;
    let body = msg.body();
    let path: ObjectPath<'_> = body.deserialize()?;
    Ok(path.as_str().to_string())
}

async fn record_niri_window(
    conn: &Connection,
    session_path: &str,
    window_id: u64,
    cursor_mode: u32,
) -> anyhow::Result<String> {
    let mut options: HashMap<&str, OwnedValue> = HashMap::new();
    options.insert("cursor-mode", OwnedValue::from(cursor_mode));
    options.insert("window-id", OwnedValue::from(window_id));
    let msg = conn
        .call_method(
            Some(MUTTER_SCREENCAST_DEST),
            session_path,
            Some("org.gnome.Mutter.ScreenCast.Session"),
            "RecordWindow",
            &options,
        )
        .await?;
    let body = msg.body();
    let path: ObjectPath<'_> = body.deserialize()?;
    Ok(path.as_str().to_string())
}

async fn start_and_get_node_id(
    conn: &Connection,
    session_path: &str,
    stream_path: &str,
) -> anyhow::Result<u32> {
    use futures_util::StreamExt;
    use tokio::time::timeout;
    use zbus::proxy::Proxy;

    let proxy = Proxy::new(
        conn,
        MUTTER_SCREENCAST_DEST,
        stream_path,
        "org.gnome.Mutter.ScreenCast.Stream",
    )
    .await?;
    let mut signal = proxy.receive_signal("PipeWireStreamAdded").await?;

    conn.call_method(
        Some(MUTTER_SCREENCAST_DEST),
        session_path,
        Some("org.gnome.Mutter.ScreenCast.Session"),
        "Start",
        &(),
    )
    .await?;

    match timeout(std::time::Duration::from_secs(10), signal.next()).await {
        Ok(Some(msg)) => {
            let (node_id,): (u32,) = msg.body().deserialize()?;
            Ok(node_id)
        }
        Ok(None) => anyhow::bail!("signal stream ended"),
        Err(_) => anyhow::bail!("timeout waiting for PipeWire node"),
    }
}

fn get_window_size(window_id: u64) -> Option<(u32, u32)> {
    niri_ipc::list_windows()
        .ok()?
        .into_iter()
        .find_map(|window| {
            (window.id == window_id)
                .then(|| positive_size(window.size.width, window.size.height))?
        })
}

fn get_output_size(output_name: &str) -> Option<(u32, u32)> {
    niri_ipc::list_outputs()
        .ok()?
        .into_iter()
        .find_map(|output| {
            (output.name == output_name)
                .then(|| positive_size(output.logical.width, output.logical.height))?
        })
}

fn positive_size(width: i32, height: i32) -> Option<(u32, u32)> {
    if width > 0 && height > 0 {
        Some((width as u32, height as u32))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_types_use_portal_types_option_semantics() {
        assert_eq!(normalize_source_types(None), Some(1));
        assert_eq!(normalize_source_types(Some(1)), Some(1));
        assert_eq!(normalize_source_types(Some(2)), Some(2));
        assert_eq!(normalize_source_types(Some(3)), Some(3));
        assert_eq!(normalize_source_types(Some(0)), None);
        assert_eq!(normalize_source_types(Some(4)), None);
        assert_eq!(normalize_source_types(Some(5)), Some(1));
    }

    #[test]
    fn cursor_defaults_to_hidden() {
        assert_eq!(portal_cursor_to_niri(None), 0);
        assert_eq!(portal_cursor_to_niri(Some(1)), 0);
        assert_eq!(portal_cursor_to_niri(Some(2)), 1);
        assert_eq!(portal_cursor_to_niri(Some(4)), 2);
        assert_eq!(portal_cursor_to_niri(Some(99)), 0);
    }

    #[test]
    fn invalid_sizes_are_not_exposed_as_wrapped_u32() {
        assert_eq!(positive_size(1920, 1080), Some((1920, 1080)));
        assert_eq!(positive_size(0, 1080), None);
        assert_eq!(positive_size(-1, 1080), None);
    }
}
