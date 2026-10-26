use tokio::sync::watch;
use zbus::zvariant::{ObjectPath, OwnedObjectPath};
use zbus::{fdo, interface, Connection};

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Cancelled,
    Finished,
}

struct RequestHandler {
    state: watch::Sender<State>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestHandler {
    fn close(&self) {
        self.state.send_if_modified(|state| {
            if *state == State::Pending {
                *state = State::Cancelled;
                true
            } else {
                false
            }
        });
    }
}

/// A pending backend call owns its Request object until it returns a response.
pub(super) struct Request {
    conn: Connection,
    path: OwnedObjectPath,
    state: watch::Sender<State>,
}

impl Request {
    pub(super) async fn export(conn: &Connection, path: ObjectPath<'_>) -> fdo::Result<Self> {
        let (state, _) = watch::channel(State::Pending);
        let registered = conn
            .object_server()
            .at(
                path.clone(),
                RequestHandler {
                    state: state.clone(),
                },
            )
            .await
            .map_err(|e| fdo::Error::Failed(format!("register request: {e}")))?;
        if !registered {
            return Err(fdo::Error::Failed("request path already registered".into()));
        }
        Ok(Self {
            conn: conn.clone(),
            path: path.into(),
            state,
        })
    }

    pub(super) async fn pending(conn: &Connection, path: ObjectPath<'_>) -> fdo::Result<Self> {
        let interface = conn
            .object_server()
            .interface::<_, RequestHandler>(path.clone())
            .await
            .map_err(|e| fdo::Error::Failed(format!("find pending request: {e}")))?;
        let state = interface.get().await.state.clone();
        Ok(Self {
            conn: conn.clone(),
            path: path.into(),
            state,
        })
    }

    pub(super) async fn cancelled(&self) {
        let mut receiver = self.state.subscribe();
        let _ = receiver.wait_for(|state| *state == State::Cancelled).await;
    }

    /// Linearize completion against Close before removing the object. A late
    /// Close cannot cancel a different request or undo a completed response.
    pub(super) async fn finish(self) -> bool {
        let mut cancelled = false;
        let finished = self.state.send_if_modified(|state| {
            if *state == State::Finished {
                return false;
            }
            cancelled = *state == State::Cancelled;
            *state = State::Finished;
            true
        });
        if !finished {
            return false;
        }
        if let Err(e) = self
            .conn
            .object_server()
            .remove::<RequestHandler, _>(&self.path)
            .await
        {
            tracing::debug!("failed to unregister request {}: {e}", self.path);
        }
        cancelled
    }
}
