use embassy_time::Instant;

use crate::{
    ConfigBuilder, Error, Property, ProtocolError, PubError, QoS, ReasonCode,
    publication::Publication, types::TopicFilter,
};

use super::{ConnectEvent, Event, Io, core::Core};

struct ConnectRollback<'a, 'buf> {
    core: &'a mut Core<'buf>,
    armed: bool,
}

impl<'a, 'buf> ConnectRollback<'a, 'buf> {
    fn new(core: &'a mut Core<'buf>) -> Self {
        Self { core, armed: true }
    }

    async fn connect<IO: Io>(&mut self, io: &mut IO) -> Result<ConnectEvent, Error<IO::Error>> {
        let result = self.core.connect(io).await;
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for ConnectRollback<'_, '_> {
    fn drop(&mut self) {
        if self.armed {
            self.core.rollback_transport();
        }
    }
}

/// One long-lived MQTT client session.
///
/// Drive the session by calling [`poll`](Self::poll) regularly after
/// [`connect`](Self::connect) has taken ownership of a live transport. The same session is also
/// used for outbound `publish`, `subscribe`, and `unsubscribe` operations.
///
/// Cancel safety, assuming the transport's I/O futures are cancel-safe:
/// [`poll`](Self::poll), [`disconnect`](Self::disconnect), [`subscribe`](Self::subscribe),
/// [`unsubscribe`](Self::unsubscribe), and [`publish`](Self::publish) for QoS 1/2 preserve local
/// session state across cancellation. [`connect`](Self::connect) rolls back to a disconnected
/// session and drops the supplied transport if cancelled. QoS 0 [`publish`](Self::publish) is not
/// cancel-safe.
pub struct Session<'buf, IO> {
    core: Core<'buf>,
    connection: Option<IO>,
}

impl<'buf, IO> Session<'buf, IO>
where
    IO: Io,
{
    async fn disconnect_slot(
        core: &mut Core<'buf>,
        slot: &mut Option<IO>,
    ) -> Result<(), Error<IO::Error>> {
        let Some(connection) = slot.as_mut() else {
            return Ok(());
        };
        let result = core.disconnect(connection).await;
        *slot = None;
        result
    }

    async fn step_slot<'a>(
        core: &'a mut Core<'buf>,
        slot: &mut Option<IO>,
    ) -> Result<Event<'a>, Error<IO::Error>> {
        if !core.is_connected() {
            return Err(Error::Disconnected);
        }
        let Some(connection) = slot.as_mut() else {
            return Err(Error::Disconnected);
        };
        match core.step_event(connection, Instant::now()).await {
            Ok(event) => Ok(event),
            Err(err) => {
                if !matches!(
                    err,
                    Error::Protocol(ProtocolError::Failed(reason))
                        if reason != ReasonCode::PacketTooLarge
                ) {
                    *slot = None;
                }
                Err(err)
            }
        }
    }

    /// Construct a session from a setup builder.
    pub fn new(config: ConfigBuilder<'buf>) -> Self {
        Self {
            core: Core::new(config),
            connection: None,
        }
    }

    /// Return whether the MQTT session is currently established.
    pub fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    /// Return whether the session currently has the local capacity to attempt a
    /// publish at the requested QoS.
    pub fn can_publish(&mut self, qos: QoS) -> bool {
        self.core.can_publish(qos)
    }

    /// Return whether the session has no in-flight retained MQTT packets or
    /// pending release state.
    pub fn is_publish_quiescent(&self) -> bool {
        self.core.is_publish_quiescent()
    }

    /// Gracefully close the current MQTT transport with `DISCONNECT`.
    ///
    /// Cancel-safe if the underlying transport write/flush futures are cancel-safe.
    pub async fn disconnect(&mut self) -> Result<(), Error<IO::Error>> {
        Self::disconnect_slot(&mut self.core, &mut self.connection).await
    }

    /// Send a `SUBSCRIBE`.
    ///
    /// Call this after [`connect`](Self::connect). A resumed [`ConnectEvent::Reconnected`]
    /// already kept broker-side subscriptions.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error<IO::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        let result = self.core.subscribe(connection, topics, properties).await;
        if self.core.is_disconnected() {
            self.connection = None;
        }
        result
    }

    /// Send an `UNSUBSCRIBE`.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn unsubscribe(
        &mut self,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<(), Error<IO::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        let result = self.core.unsubscribe(connection, topics, properties).await;
        if self.core.is_disconnected() {
            self.connection = None;
        }
        result
    }

    /// Send a `PUBLISH`.
    ///
    /// QoS 1 and 2 retain the encoded packet in the session TX buffer until broker ack and are
    /// cancel-safe if the underlying transport I/O futures are cancel-safe.
    ///
    /// QoS 0 bypasses retained outbound state, encodes into temporary TX scratch space, and writes
    /// directly to the transport. It therefore does not consume replay/in-flight slots and only
    /// needs enough currently free TX space for that one encode, but it is not cancel-safe.
    pub async fn publish<P>(
        &mut self,
        publication: Publication<'_, P>,
    ) -> Result<(), PubError<P::Error, IO::Error>>
    where
        P: crate::publication::ToPayload,
    {
        let Some(connection) = self.connection.as_mut() else {
            return Err(PubError::Session(Error::Disconnected));
        };
        let result = self.core.publish(connection, publication).await;
        if self.core.is_disconnected() {
            self.connection = None;
        }
        result
    }

    /// Establish or resume the MQTT session on a newly supplied transport.
    ///
    /// The session takes ownership of `io` for the lifetime of the connected session and drops it
    /// again on disconnect or connection failure. If cancelled during the handshake, the session
    /// rolls back to disconnected state and drops `io`.
    pub async fn connect(&mut self, mut io: IO) -> Result<ConnectEvent, Error<IO::Error>> {
        if self.connection.is_some() || self.core.is_connected() {
            return Err(Error::NotReady);
        }
        let result = {
            let mut rollback = ConnectRollback::new(&mut self.core);
            rollback.connect(&mut io).await
        };
        if result.is_ok() {
            self.connection = Some(io);
        }
        result
    }

    /// Advance the session once.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn poll(&mut self) -> Result<Event<'_>, Error<IO::Error>> {
        self.step().await
    }

    /// Advance the session once without blocking on idle transport reads.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn step(&mut self) -> Result<Event<'_>, Error<IO::Error>> {
        Self::step_slot(&mut self.core, &mut self.connection).await
    }
}
