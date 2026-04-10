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

    /// Return whether the session currently has local transport and in-flight capacity for a
    /// publish at the requested QoS.
    ///
    /// This is intentionally a local readiness check. It does not account for payload-dependent
    /// serialization failures or broker-advertised `MaximumPacketSize`, so `publish()` remains the
    /// authoritative operation.
    pub fn is_publish_ready(&mut self, qos: QoS) -> bool {
        self.core.is_publish_ready(qos)
    }

    /// Gracefully close the current MQTT transport with `DISCONNECT`.
    ///
    /// This only closes the current transport. A later [`poll`](Self::poll) will connect again.
    /// If the broker preserves the MQTT session, subscriptions and in-flight state may resume.
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        let Some(mut connection) = self.connection.take() else {
            self.pending_activation = None;
            return Ok(());
        };
        let result = self.core.disconnect(&mut connection).await;
        self.pending_activation = None;
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
        let mut connection = self.take_connection()?;
        let result = self
            .core
            .subscribe(&mut connection, topics, properties)
            .await;
        self.connection = Some(connection);
        result
    }

    /// Send an `UNSUBSCRIBE`.
    pub async fn unsubscribe(
        &mut self,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<(), Error> {
        let _ = self.ensure_connected().await?;
        let mut connection = self.take_connection()?;
        let result = self
            .core
            .unsubscribe(&mut connection, topics, properties)
            .await;
        self.connection = Some(connection);
        result
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
        let mut connection = self.take_connection().map_err(PubError::Error)?;
        let result = self.core.publish(&mut connection, publication).await;
        self.connection = Some(connection);
        result
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
                self.core.reset_reader();
                self.connection = None;
            }

            if self.connection.is_none() {
                let connection = self.connector.connect(self.core.broker()).await?;
                self.connection = Some(connection);
                let mut connection = self.take_connection()?;
                let result = self.core.connect(&mut connection).await;
                self.connection = Some(connection);
                result?;
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
        {
            let mut connection = self.take_connection()?;
            let result = self.core.maintain(&mut connection, now).await;
            self.connection = Some(connection);
            result?;
        }

        if self.core.is_disconnected() {
            self.connection = None;
            return Ok(None);
        }

        if let Some(deadline) = self.core.next_deadline() {
            if deadline <= Instant::now() {
                let mut connection = self.take_connection()?;
                let result = self.core.maintain(&mut connection, Instant::now()).await;
                self.connection = Some(connection);
                result?;
                if self.core.is_disconnected() {
                    self.connection = None;
                }
                return Ok(None);
            }

            let mut connection = self.take_connection()?;
            let result =
                match select(self.core.read(&mut connection, now), Timer::at(deadline)).await {
                    Either::First(result) => match result {
                        Ok(result) => result,
                        Err(Error::Transport(ErrorKind::TimedOut | ErrorKind::Interrupted)) => {
                            self.connection = Some(connection);
                            return Ok(None);
                        }
                        Err(err) => {
                            self.connection = Some(connection);
                            return Err(err);
                        }
                    },
                    Either::Second(_) => {
                        self.connection = Some(connection);
                        return Ok(None);
                    }
                };
            self.connection = Some(connection);
            return Ok(result);
        }

        let mut connection = self.take_connection()?;
        let result = match self.core.read(&mut connection, now).await {
            Ok(result) => result,
            Err(Error::Transport(ErrorKind::TimedOut | ErrorKind::Interrupted)) => {
                self.connection = Some(connection);
                return Ok(None);
            }
            Err(err) => {
                self.connection = Some(connection);
                return Err(err);
            }
        };
        self.connection = Some(connection);
        Ok(result)
    }

    fn take_connection(&mut self) -> Result<C::Connection<'a>, Error> {
        debug_assert!(
            self.connection.is_some(),
            "connection missing while session is active"
        );
        self.connection.take().ok_or(Error::Disconnected)
    }
}
