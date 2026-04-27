use embassy_time::Instant;

use crate::{
    ConfigBuilder, Error, Property, ProtocolError, PubError, QoS, ReasonCode,
    publication::Publication, types::TopicFilter,
};

use super::{ConnectEvent, Event, Io, core::Core};

/// One long-lived MQTT client session.
///
/// Drive the session by calling [`poll`](Self::poll) regularly after
/// [`connect`](Self::connect) has taken ownership of a live transport. The same session is also
/// used for outbound `publish`, `subscribe`, and `unsubscribe` operations.
pub struct Session<'buf, IO> {
    core: Core<'buf>,
    connection: Option<IO>,
}

impl<'buf, IO> Session<'buf, IO>
where
    IO: Io,
{
    fn keeps_transport(err: &Error<IO::Error>) -> bool {
        matches!(
            err,
            Error::Protocol(ProtocolError::Failed(reason)) if *reason != ReasonCode::PacketTooLarge
        )
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

    fn clear_disconnected_transport(&mut self) {
        if self.core.is_disconnected() {
            self.connection = None;
        }
    }

    /// Gracefully close the current MQTT transport with `DISCONNECT`.
    pub async fn disconnect(&mut self) -> Result<(), Error<IO::Error>> {
        let Some(mut connection) = self.connection.take() else {
            return Ok(());
        };
        let result = self.core.disconnect(&mut connection).await;
        self.clear_disconnected_transport();
        result
    }

    /// Send a `SUBSCRIBE`.
    ///
    /// Call this after [`connect`](Self::connect). A resumed [`ConnectEvent::Reconnected`]
    /// already kept broker-side subscriptions.
    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error<IO::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        let result = self.core.subscribe(connection, topics, properties).await;
        self.clear_disconnected_transport();
        result
    }

    /// Send an `UNSUBSCRIBE`.
    pub async fn unsubscribe(
        &mut self,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<(), Error<IO::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        let result = self.core.unsubscribe(connection, topics, properties).await;
        self.clear_disconnected_transport();
        result
    }

    /// Send a `PUBLISH`.
    ///
    /// For QoS 1 and 2, the session retains the encoded packet in its TX buffer until the broker
    /// acknowledges it.
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
        self.clear_disconnected_transport();
        result
    }

    /// Establish or resume the MQTT session on a newly supplied transport.
    ///
    /// The session takes ownership of `io` for the lifetime of the connected session and drops it
    /// again on disconnect or connection failure.
    pub async fn connect(&mut self, io: IO) -> Result<ConnectEvent, Error<IO::Error>> {
        if self.connection.is_some() || self.core.is_connected() {
            return Err(Error::NotReady);
        }
        self.connection = Some(io);
        let result = {
            let connection = self.connection.as_mut().unwrap();
            self.core.connect(connection).await
        };
        if result.is_err() {
            self.connection = None;
        }
        result
    }

    /// Advance the session once.
    pub async fn poll(&mut self) -> Result<Event<'_>, Error<IO::Error>> {
        self.step().await
    }

    /// Advance the session once without blocking on idle transport reads.
    pub async fn step(&mut self) -> Result<Event<'_>, Error<IO::Error>> {
        if !self.core.is_connected() {
            return Err(Error::Disconnected);
        }
        let Some(mut connection) = self.connection.take() else {
            return Err(Error::Disconnected);
        };
        match self.core.step_event(&mut connection, Instant::now()).await {
            Ok(event) => {
                self.connection = Some(connection);
                Ok(event)
            }
            Err(err) => {
                if Self::keeps_transport(&err) {
                    self.connection = Some(connection);
                }
                Err(err)
            }
        }
    }
}
