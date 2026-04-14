use embassy_futures::select::{Either, select};
use embassy_time::{Instant, Timer};
use embedded_io_async::ErrorKind;

use crate::{
    Config, Error, Property, PubError, QoS, publication::Publication, transport::Connector,
    types::TopicFilter,
};

use super::{Event, InboundPublish, core::Core};

/// One long-lived MQTT client session.
///
/// Drive the session by calling [`poll`](Self::poll) regularly. The same session is also used for
/// outbound `publish`, `subscribe`, and `unsubscribe` operations.
pub struct Session<'a, 'buf, C: Connector> {
    core: Core<'buf>,
    connector: &'a C,
    connection: Option<C::Connection<'a>>,
    pending_activation: Option<bool>,
}

impl<'a, 'buf, C> Session<'a, 'buf, C>
where
    C: Connector,
{
    /// Construct a session from a built [`Config`](crate::Config) and transport connector.
    pub fn new(config: Config<'buf>, connector: &'a C) -> Self {
        Self {
            core: Core::new(config),
            connector,
            connection: None,
            pending_activation: None,
        }
    }

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

    /// Gracefully close the current MQTT transport with `DISCONNECT`.
    ///
    /// This only closes the current transport. A later [`poll`](Self::poll) will connect again.
    /// If the broker preserves the MQTT session, subscriptions and in-flight state may resume.
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        self.pending_activation = None;
        let Some(connection) = self.connection.as_mut() else {
            return Ok(());
        };
        let result = self.core.disconnect(connection).await;
        self.connection = None;
        result
    }

    /// Send a `SUBSCRIBE`.
    ///
    /// Call this after [`Event::Connected`](crate::Event::Connected). A resumed
    /// [`Event::Reconnected`](crate::Event::Reconnected) already kept broker-side subscriptions.
    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error> {
        let _ = self.ensure_connected().await?;
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
    ) -> Result<(), Error> {
        let _ = self.ensure_connected().await?;
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
    ) -> Result<(), PubError<P::Error>>
    where
        P: crate::publication::ToPayload,
    {
        let _ = self.ensure_connected().await.map_err(PubError::Error)?;
        let Some(connection) = self.connection.as_mut() else {
            return Err(PubError::Error(Error::Disconnected));
        };
        self.core.publish(connection, publication).await
    }

    /// Advance the session once.
    ///
    /// `poll()` drives reconnect, keepalive, retransmission, and inbound packet handling. Call it
    /// regularly from your main loop.
    pub async fn poll(&mut self) -> Result<Event<'_>, Error> {
        let _ = self.ensure_connected().await?;
        if let Some(resumed) = self.pending_activation.take() {
            return Ok(if resumed {
                Event::Reconnected
            } else {
                Event::Connected
            });
        }

        match self.step().await? {
            Some(inbound) => Ok(Event::Inbound(inbound)),
            None => Ok(Event::Idle),
        }
    }

    async fn ensure_connected(&mut self) -> Result<bool, Error> {
        let mut activated = false;
        loop {
            if self.core.is_disconnected() {
                self.connection = None;
            }

            if self.connection.is_none() {
                let connection = self.connector.connect(self.core.broker()).await?;
                self.connection = Some(connection);
                let Some(connection) = self.connection.as_mut() else {
                    return Err(Error::Disconnected);
                };
                self.core.connect(connection).await?;
                activated = true;
            }

            if self.core.is_connected() {
                if activated {
                    self.pending_activation = Some(self.core.session_resumed());
                }
                return Ok(activated);
            }

            let _ = self.step().await?;
        }
    }

    async fn step(&mut self) -> Result<Option<InboundPublish<'_>>, Error> {
        let now = Instant::now();
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        self.core.maintain(connection, now).await?;

        if self.core.is_disconnected() {
            self.connection = None;
            return Ok(None);
        }

        if let Some(deadline) = self.core.next_deadline() {
            if deadline <= Instant::now() {
                let Some(connection) = self.connection.as_mut() else {
                    return Err(Error::Disconnected);
                };
                self.core.maintain(connection, Instant::now()).await?;
                if self.core.is_disconnected() {
                    self.connection = None;
                }
                return Ok(None);
            }

            let Some(connection) = self.connection.as_mut() else {
                return Err(Error::Disconnected);
            };
            return match select(self.core.read(connection, now), Timer::at(deadline)).await {
                Either::First(result) => match result {
                    Ok(result) => Ok(result),
                    Err(Error::Transport(ErrorKind::TimedOut | ErrorKind::Interrupted)) => Ok(None),
                    Err(err) => Err(err),
                },
                Either::Second(_) => Ok(None),
            };
        }

        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        let result = match self.core.read(connection, now).await {
            Ok(result) => result,
            Err(Error::Transport(ErrorKind::TimedOut | ErrorKind::Interrupted)) => return Ok(None),
            Err(err) => return Err(err),
        };
        Ok(result)
    }
}
