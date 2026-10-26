use std::collections::HashMap;
use std::sync::Arc;

use zbus::export::async_trait::async_trait;
use zbus::message::{Header, Message};
use zbus::names::{InterfaceName, MemberName};
use zbus::object_server::{DispatchResult2, Interface, SignalEmitter};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{fdo, Connection, ObjectServer};

use super::{Request, ScreenCastInterface};

/// zbus normally spawns a task before running a method body, so a following
/// Request.Close can be dispatched before that body exports its Request object.
/// Register requests in message order, then delegate the actual method and its
/// reply to the generated interface. Properties and introspection stay generated.
pub(super) struct OrderedScreenCast(Arc<ScreenCastInterface>);

impl OrderedScreenCast {
    pub(super) fn new(interface: ScreenCastInterface) -> Self {
        Self(Arc::new(interface))
    }
}

#[async_trait]
impl Interface for OrderedScreenCast {
    fn name() -> InterfaceName<'static> {
        ScreenCastInterface::name()
    }

    fn spawn_tasks_for_methods(&self) -> bool {
        false
    }

    async fn get(
        &self,
        name: &str,
        server: &ObjectServer,
        conn: &Connection,
        header: Option<&Header<'_>>,
        emitter: &SignalEmitter<'_>,
    ) -> Option<fdo::Result<OwnedValue>> {
        self.0.get(name, server, conn, header, emitter).await
    }

    async fn get_all(
        &self,
        server: &ObjectServer,
        conn: &Connection,
        header: Option<&Header<'_>>,
        emitter: &SignalEmitter<'_>,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        self.0.get_all(server, conn, header, emitter).await
    }

    fn set<'a>(
        &'a self,
        name: &'a str,
        value: &'a Value<'_>,
        server: &'a ObjectServer,
        conn: &'a Connection,
        header: Option<&'a Header<'_>>,
        emitter: &'a SignalEmitter<'_>,
    ) -> DispatchResult2<'a> {
        self.0.set(name, value, server, conn, header, emitter)
    }

    async fn set_mut(
        &mut self,
        _name: &str,
        _value: &Value<'_>,
        _server: &ObjectServer,
        _conn: &Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> Option<fdo::Result<()>> {
        // ScreenCast has no writable properties.
        None
    }

    fn call<'a>(
        &'a self,
        _server: &'a ObjectServer,
        conn: &'a Connection,
        message: &'a Message,
        name: MemberName<'a>,
    ) -> DispatchResult2<'a> {
        if !matches!(name.as_str(), "CreateSession" | "SelectSources" | "Start") {
            return DispatchResult2::NotFound;
        }
        DispatchResult2::Async(Box::pin(async move {
            // All three methods begin with a request object path. Decode only
            // that borrowed field here; generated dispatch validates the body.
            let body = message.body();
            let (handle, _) = body
                .data()
                .deserialize::<ObjectPath<'_>>()
                .map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
            let request = Request::export(conn, handle).await?;
            let interface = self.0.clone();
            let conn = conn.clone();
            let message = message.clone();
            tokio::spawn(async move {
                let header = message.header();
                let member = header.member().expect("dispatcher checked the member");
                let result =
                    match interface.call(conn.object_server(), &conn, &message, member.as_ref()) {
                        DispatchResult2::Async(future) => future.await,
                        _ => Err(fdo::Error::UnknownMethod(member.to_string())),
                    };
                if let Err(error) = result {
                    if let Err(error) = conn.reply_dbus_error(&header, error).await {
                        tracing::debug!("failed to reply to ScreenCast call: {error}");
                    }
                }
                // Also clean up if argument validation failed before the
                // generated method could take ownership of its Request.
                request.finish().await;
            });
            Ok(())
        }))
    }

    fn call_mut<'a>(
        &'a mut self,
        _server: &'a ObjectServer,
        _conn: &'a Connection,
        _message: &'a Message,
        _name: MemberName<'a>,
    ) -> DispatchResult2<'a> {
        // All generated handlers use shared access to their synchronized state.
        DispatchResult2::NotFound
    }

    fn introspect_to_writer(&self, writer: &mut dyn std::fmt::Write, level: usize) {
        self.0.introspect_to_writer(writer, level);
    }
}
