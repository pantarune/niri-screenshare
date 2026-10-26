use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::timeout;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedObjectPath;

const BACKEND: &str = "org.freedesktop.impl.portal.desktop.niri";
const BASE: &str = "/org/freedesktop/portal/desktop";
const SESSION: &str = "/org/freedesktop/portal/desktop/session/test/capture";
const START: &str = "/org/freedesktop/portal/desktop/request/test/start";
const COMPOSITOR_SESSION: &str = "/org/gnome/Mutter/ScreenCast/Session/test";
const STREAM: &str = "/org/gnome/Mutter/ScreenCast/Stream/test";
const DEADLINE: Duration = Duration::from_secs(5);

type Response = (u32, HashMap<String, OwnedValue>);

struct TestBus(Child);

impl TestBus {
    fn start() -> (Self, String) {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("D-Bus tests require dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        (Self(child), address.trim().to_owned())
    }
}

impl Drop for TestBus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Compositor;

#[interface(name = "org.gnome.Mutter.ScreenCast")]
impl Compositor {
    fn create_session(&self, _options: HashMap<String, OwnedValue>) -> OwnedObjectPath {
        COMPOSITOR_SESSION.try_into().unwrap()
    }
}

struct CompositorSession {
    publish_node: bool,
    started: Arc<Notify>,
    stops: Arc<AtomicUsize>,
}

#[interface(name = "org.gnome.Mutter.ScreenCast.Session")]
impl CompositorSession {
    fn record_monitor(
        &self,
        _output: &str,
        _options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        STREAM.try_into().unwrap()
    }

    async fn start(&self, #[zbus(connection)] conn: &Connection) -> fdo::Result<()> {
        if self.publish_node {
            let emitter = SignalEmitter::new(conn, STREAM)?;
            CompositorStream::pipe_wire_stream_added(&emitter, 42).await?;
        }
        self.started.notify_one();
        Ok(())
    }

    async fn stop(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> fdo::Result<()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Self::closed(&emitter).await.map_err(Into::into)
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

struct CompositorStream;

#[interface(name = "org.gnome.Mutter.ScreenCast.Stream")]
impl CompositorStream {
    #[zbus(signal)]
    async fn pipe_wire_stream_added(emitter: &SignalEmitter<'_>, node_id: u32) -> zbus::Result<()>;
}

struct Fixture {
    server: Connection,
    client: Connection,
    sessions: Arc<Mutex<HashMap<String, CaptureSession>>>,
    started: Arc<Notify>,
    stops: Arc<AtomicUsize>,
    _bus: TestBus,
}

impl Fixture {
    async fn new(publish_node: bool) -> Self {
        let (bus, address) = TestBus::start();
        let server = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name(BACKEND)
            .unwrap()
            .name(MUTTER_SCREENCAST_DEST)
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let portal = ScreenCastInterface::new(server.clone());
        let sessions = portal.state.clone();
        let started = Arc::new(Notify::new());
        let stops = Arc::new(AtomicUsize::new(0));
        server
            .object_server()
            .at(BASE, dispatch::OrderedScreenCast::new(portal))
            .await
            .unwrap();
        server
            .object_server()
            .at(MUTTER_SCREENCAST_PATH, Compositor)
            .await
            .unwrap();
        server
            .object_server()
            .at(
                COMPOSITOR_SESSION,
                CompositorSession {
                    publish_node,
                    started: started.clone(),
                    stops: stops.clone(),
                },
            )
            .await
            .unwrap();
        server
            .object_server()
            .at(STREAM, CompositorStream)
            .await
            .unwrap();
        Self {
            server,
            client,
            sessions,
            started,
            stops,
            _bus: bus,
        }
    }

    async fn create(&self) {
        let response: Response = self
            .client
            .call_method(
                Some(BACKEND),
                BASE,
                Some("org.freedesktop.impl.portal.ScreenCast"),
                "CreateSession",
                &(
                    ObjectPath::try_from("/org/freedesktop/portal/desktop/request/test/create")
                        .unwrap(),
                    ObjectPath::try_from(SESSION).unwrap(),
                    "test.app",
                    HashMap::<String, OwnedValue>::new(),
                ),
            )
            .await
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        assert_eq!(response.0, 0);
        // This fixture exercises Start and lifecycle independently of a GUI or
        // real compositor; seed the target normally supplied by SelectSources.
        self.sessions
            .lock()
            .await
            .get_mut(SESSION)
            .unwrap()
            .output_name = Some("TEST-OUTPUT".into());
    }

    fn start(&self) -> tokio::task::JoinHandle<Response> {
        let client = self.client.clone();
        tokio::spawn(async move {
            client
                .call_method(
                    Some(BACKEND),
                    BASE,
                    Some("org.freedesktop.impl.portal.ScreenCast"),
                    "Start",
                    &(
                        ObjectPath::try_from(START).unwrap(),
                        ObjectPath::try_from(SESSION).unwrap(),
                        "test.app",
                        "",
                        HashMap::<String, OwnedValue>::new(),
                    ),
                )
                .await
                .unwrap()
                .body()
                .deserialize()
                .unwrap()
        })
    }

    async fn close(&self, path: &str, interface: &str) {
        timeout(
            DEADLINE,
            self.client
                .call_method(Some(BACKEND), path, Some(interface), "Close", &()),
        )
        .await
        .unwrap()
        .unwrap();
    }

    async fn assert_unexported(&self, path: &str, interface: &str) {
        let reply = self
            .client
            .call_method(
                Some(BACKEND),
                path,
                Some("org.freedesktop.DBus.Introspectable"),
                "Introspect",
                &(),
            )
            .await;
        match reply {
            Ok(reply) => {
                let xml: String = reply.body().deserialize().unwrap();
                assert!(!xml.contains(interface), "object still exported: {xml}");
            }
            Err(zbus::Error::MethodError(name, _, _)) => {
                assert_eq!(name.as_str(), "org.freedesktop.DBus.Error.UnknownObject")
            }
            Err(error) => panic!("unexpected introspection failure: {error}"),
        }
    }
}

#[tokio::test]
async fn request_close_is_sticky_and_does_not_cancel_another_request() {
    let fixture = Fixture::new(false).await;
    let first = Request::export(&fixture.server, ObjectPath::try_from(START).unwrap())
        .await
        .unwrap();
    let second_path = "/org/freedesktop/portal/desktop/request/test/other";
    let second = Request::export(&fixture.server, ObjectPath::try_from(second_path).unwrap())
        .await
        .unwrap();
    fixture
        .close(START, "org.freedesktop.impl.portal.Request")
        .await;
    // Subscribe after Close to cover cancellation before the worker starts.
    timeout(DEADLINE, first.cancelled()).await.unwrap();
    assert!(first.finish().await);
    assert!(!second.finish().await);
    fixture
        .assert_unexported(START, "org.freedesktop.impl.portal.Request")
        .await;
    fixture
        .assert_unexported(second_path, "org.freedesktop.impl.portal.Request")
        .await;
}

#[tokio::test]
async fn request_close_during_start_stops_compositor_and_removes_session() {
    let fixture = Fixture::new(false).await;
    fixture.create().await;
    let start = fixture.start();
    timeout(DEADLINE, fixture.started.notified()).await.unwrap();
    fixture
        .close(START, "org.freedesktop.impl.portal.Request")
        .await;
    let response = timeout(DEADLINE, start).await.unwrap().unwrap();
    assert_eq!(response.0, 1);
    assert!(!response.1.contains_key("streams"));
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
    assert!(!fixture.sessions.lock().await.contains_key(SESSION));
    fixture
        .assert_unexported(START, "org.freedesktop.impl.portal.Request")
        .await;
    fixture
        .assert_unexported(SESSION, "org.freedesktop.impl.portal.Session")
        .await;
}

#[tokio::test]
async fn compositor_close_notifies_frontend_and_removes_session() {
    let fixture = Fixture::new(true).await;
    fixture.create().await;
    let session = zbus::Proxy::new(
        &fixture.client,
        BACKEND,
        SESSION,
        "org.freedesktop.impl.portal.Session",
    )
    .await
    .unwrap();
    assert_eq!(session.get_property::<u32>("version").await.unwrap(), 1);
    let mut closed = session.receive_signal("Closed").await.unwrap();
    let response = timeout(DEADLINE, fixture.start()).await.unwrap().unwrap();
    assert_eq!(response.0, 0);
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = response.1["streams"]
        .try_clone()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(streams[0].0, 42);
    let emitter = SignalEmitter::new(&fixture.server, COMPOSITOR_SESSION).unwrap();
    CompositorSession::closed(&emitter).await.unwrap();
    timeout(DEADLINE, closed.next())
        .await
        .unwrap()
        .expect("missing Session.Closed");
    // The signal is emitted before unexporting; synchronizing on the object
    // server ensures the close task has completed without a timing sleep.
    timeout(DEADLINE, async {
        loop {
            if fixture
                .server
                .object_server()
                .interface::<_, SessionHandler>(SESSION)
                .await
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!fixture.sessions.lock().await.contains_key(SESSION));
    fixture
        .assert_unexported(SESSION, "org.freedesktop.impl.portal.Session")
        .await;
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn session_close_during_start_cancels_without_leaking_capture() {
    let fixture = Fixture::new(false).await;
    fixture.create().await;
    let start = fixture.start();
    timeout(DEADLINE, fixture.started.notified()).await.unwrap();
    fixture
        .close(SESSION, "org.freedesktop.impl.portal.Session")
        .await;
    assert_eq!(timeout(DEADLINE, start).await.unwrap().unwrap().0, 1);
    assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
    fixture
        .assert_unexported(SESSION, "org.freedesktop.impl.portal.Session")
        .await;
    fixture
        .assert_unexported(START, "org.freedesktop.impl.portal.Request")
        .await;
}

#[tokio::test]
async fn close_immediately_after_start_does_not_race_request_registration() {
    let fixture = Fixture::new(false).await;
    fixture.create().await;
    let start = zbus::Message::method_call(BASE, "Start")
        .unwrap()
        .destination(BACKEND)
        .unwrap()
        .interface("org.freedesktop.impl.portal.ScreenCast")
        .unwrap()
        .build(&(
            ObjectPath::try_from(START).unwrap(),
            ObjectPath::try_from(SESSION).unwrap(),
            "test.app",
            "",
            HashMap::<String, OwnedValue>::new(),
        ))
        .unwrap();
    let close = zbus::Message::method_call(START, "Close")
        .unwrap()
        .destination(BACKEND)
        .unwrap()
        .interface("org.freedesktop.impl.portal.Request")
        .unwrap()
        .build(&())
        .unwrap();
    let mut replies = zbus::MessageStream::from(&fixture.client);
    // Do not wait for a picker, node or backend method to start. The object
    // server must export the request before dispatching the very next message.
    fixture.client.send(&start).await.unwrap();
    fixture.client.send(&close).await.unwrap();
    timeout(DEADLINE, async {
        let mut got_start = false;
        let mut got_close = false;
        while !got_start || !got_close {
            let reply = replies.next().await.unwrap().unwrap();
            let serial = reply.header().reply_serial();
            if serial == Some(close.primary_header().serial_num()) {
                assert_eq!(reply.message_type(), zbus::message::Type::MethodReturn);
                reply.body().deserialize::<()>().unwrap();
                got_close = true;
            } else if serial == Some(start.primary_header().serial_num()) {
                let response: Response = reply.body().deserialize().unwrap();
                assert_eq!(response.0, 1);
                assert!(!response.1.contains_key("streams"));
                got_start = true;
            }
        }
    })
    .await
    .unwrap();
    assert!(!fixture.sessions.lock().await.contains_key(SESSION));
    fixture
        .assert_unexported(START, "org.freedesktop.impl.portal.Request")
        .await;
}
