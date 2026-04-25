use embassy_time::Instant;

use crate::{
    ConfigBuilder, Error, Property, PubError, QoS, publication::Publication, transport::Connector,
    types::TopicFilter,
};

use super::{ConnectEvent, Event, core::Core};

/// One long-lived MQTT client session.
///
/// Drive the session by calling [`poll`](Self::poll) regularly. The same session is also used for
/// outbound `publish`, `subscribe`, and `unsubscribe` operations.
pub struct Session<'a, 'buf, C: Connector> {
    core: Core<'buf>,
    connector: &'a C,
    connection: Option<C::Connection<'a>>,
}

impl<'a, 'buf, C> Session<'a, 'buf, C>
where
    C: Connector,
{
    /// Construct a session from a setup builder and transport connector.
    pub fn new(config: ConfigBuilder<'buf>, connector: &'a C) -> Self {
        Self {
            core: Core::new(config),
            connector,
            connection: None,
        }
    }

    /// Return whether the MQTT session is currently established.
    ///
    /// This does not force a connection attempt.
    pub fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    /// Return whether the session currently has the local capacity to attempt a
    /// publish at the requested QoS.
    ///
    /// This check is pessimistic for local backpressure: if it returns `false`,
    /// `publish()` would currently fail with local readiness/backpressure.
    ///
    /// It is optimistic overall: if it returns `true`, `publish()` can still
    /// fail for payload-dependent serialization, broker-advertised
    /// `MaximumPacketSize`, disconnects, or transport errors.
    pub fn can_publish(&mut self, qos: QoS) -> bool {
        self.core.can_publish(qos)
    }

    /// Return whether the session has no in-flight retained MQTT packets or
    /// pending release state.
    ///
    /// Use this when a caller needs strict one-at-a-time QoS progress, for
    /// example to serialize retained background publication.
    pub fn is_publish_quiescent(&self) -> bool {
        self.core.is_publish_quiescent()
    }

    /// Gracefully close the current MQTT transport with `DISCONNECT`.
    ///
    /// This only closes the current transport. Call [`connect`](Self::connect) again to resume or
    /// create a new broker session.
    pub async fn disconnect(&mut self) -> Result<(), Error<C::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Ok(());
        };
        let result = self.core.disconnect(connection).await;
        self.connection = None;
        result
    }

    /// Send a `SUBSCRIBE`.
    ///
    /// Call this after [`connect`](Self::connect). A resumed
    /// [`ConnectEvent::Reconnected`] already kept broker-side subscriptions.
    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error<C::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        self.core.subscribe(connection, topics, properties).await
    }

    /// Send an `UNSUBSCRIBE`.
    pub async fn unsubscribe(
        &mut self,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<(), Error<C::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        self.core.unsubscribe(connection, topics, properties).await
    }

    /// Send a `PUBLISH`.
    ///
    /// For QoS 1 and 2, the session retains the encoded packet in its TX buffer until the broker
    /// acknowledges it.
    pub async fn publish<P>(
        &mut self,
        publication: Publication<'_, P>,
    ) -> Result<(), PubError<P::Error, C::Error>>
    where
        P: crate::publication::ToPayload,
    {
        let Some(connection) = self.connection.as_mut() else {
            return Err(PubError::Session(Error::Disconnected));
        };
        self.core.publish(connection, publication).await
    }

    /// Establish or resume the MQTT session.
    pub async fn connect(&mut self) -> Result<ConnectEvent, Error<C::Error>> {
        if self.core.is_disconnected() {
            self.connection = None;
        }
        if self.connection.is_none() {
            let connection = self
                .connector
                .connect(self.core.broker())
                .await
                .map_err(Error::Transport)?;
            self.connection = Some(connection);
        }
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        match self.core.connect(connection).await {
            Ok(event) => Ok(event),
            Err(err) => {
                if self.core.is_disconnected() {
                    self.connection = None;
                }
                Err(err)
            }
        }
    }

    /// Advance the session once.
    ///
    /// `poll()` drives keepalive, retransmission, and inbound packet handling on an established
    /// session. Call it regularly from your main loop after [`connect`](Self::connect).
    pub async fn poll(&mut self) -> Result<Event<'_>, Error<C::Error>> {
        self.step().await
    }

    /// Advance the session once without blocking on idle transport reads.
    pub async fn step(&mut self) -> Result<Event<'_>, Error<C::Error>> {
        if self.core.is_disconnected() {
            self.connection = None;
            return Err(Error::Disconnected);
        }
        if !self.core.is_connected() {
            return Err(Error::Disconnected);
        }
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        self.core.step_event(connection, Instant::now()).await
    }
}
