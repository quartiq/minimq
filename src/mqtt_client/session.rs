use embassy_futures::select::{Either, select};
use embassy_time::{Instant, Timer};
use embedded_io_async::ErrorKind;

use crate::{
    Config, Error, Property, PubError, QoS, publication::Publication, transport::Connector,
    types::TopicFilter,
};

use super::{
    Event, InboundPublish,
    core::{Core, State},
};

pub struct Session<'a, 'buf, C: Connector> {
    core: Core<'buf>,
    connector: &'a C,
    connection: Option<C::Connection<'a>>,
    connected_once: bool,
}

impl<'a, 'buf, C> Session<'a, 'buf, C>
where
    C: Connector,
{
    pub fn new(config: Config<'buf>, connector: &'a C) -> Self {
        Self {
            core: Core::new(config),
            connector,
            connection: None,
            connected_once: false,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.core.is_connected()
    }

    pub fn can_publish(&mut self, qos: QoS) -> bool {
        self.core.can_publish(qos)
    }

    pub fn subscriptions_pending(&self) -> bool {
        self.core.subscriptions_pending()
    }

    pub fn pending_messages(&self) -> bool {
        self.core.pending_messages()
    }

    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error> {
        let activated = self.ensure_connected().await?;
        if activated {
            self.connected_once = true;
        }
        let mut connection = self.take_connection()?;
        let result = self
            .core
            .subscribe(&mut connection, topics, properties)
            .await;
        self.connection = Some(connection);
        result
    }

    pub async fn publish<P>(
        &mut self,
        publication: Publication<'_, P>,
    ) -> Result<(), PubError<P::Error>>
    where
        P: crate::publication::ToPayload,
    {
        let activated = self.ensure_connected().await.map_err(PubError::Error)?;
        if activated {
            self.connected_once = true;
        }
        let mut connection = self.take_connection().map_err(PubError::Error)?;
        let result = self.core.publish(&mut connection, publication).await;
        self.connection = Some(connection);
        result
    }

    pub async fn poll(&mut self) -> Result<Event<'_>, Error> {
        let reconnected = self.ensure_connected().await?;
        if reconnected {
            return Ok(if self.connected_once {
                Event::Reconnected
            } else {
                self.connected_once = true;
                Event::Connected
            });
        }

        let now = Instant::now();
        match self.step(now).await? {
            Some(inbound) => Ok(Event::Inbound(inbound)),
            None => Ok(Event::Idle),
        }
    }

    async fn ensure_connected(&mut self) -> Result<bool, Error> {
        let mut reconnected = false;
        loop {
            if self.core.state == State::Disconnected {
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
                reconnected = true;
            }

            if self.core.is_connected() {
                return Ok(reconnected);
            }

            let now = Instant::now();
            let _ = self.step(now).await?;
        }
    }

    async fn step(&mut self, now: Instant) -> Result<Option<InboundPublish<'_>>, Error> {
        {
            let mut connection = self.take_connection()?;
            let result = self.core.maintain(&mut connection, now).await;
            self.connection = Some(connection);
            result?;
        }

        if self.core.state == State::Disconnected {
            self.connection = None;
            return Ok(None);
        }

        if let Some(deadline) = self.core.next_deadline() {
            let mut connection = self.take_connection()?;
            let read = self.core.read(&mut connection, now);
            let result = match select(read, Timer::at(deadline)).await {
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
