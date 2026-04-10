use embassy_futures::select::{Either, select};
use embassy_time::{Instant, Timer};
use embedded_io_async::ErrorKind;

use crate::{
    Config, Error, Property, PubError, QoS, publication::Publication, transport::Connector,
    types::TopicFilter,
};

use super::{Event, InboundPublish, core::Core};

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

    pub fn can_publish(&mut self, qos: QoS) -> bool {
        self.core.can_publish(qos)
    }

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

    pub async fn poll(&mut self) -> Result<Event<'_>, Error> {
        let _ = self.ensure_connected().await?;
        if let Some(first_connect) = self.pending_activation.take() {
            return Ok(if first_connect {
                Event::Connected
            } else {
                Event::Reconnected
            });
        }

        match self.step().await? {
            Some(inbound) => Ok(Event::Inbound(inbound)),
            None => Ok(Event::Idle),
        }
    }

    async fn ensure_connected(&mut self) -> Result<Option<bool>, Error> {
        let mut activated = None;
        loop {
            if self.core.is_disconnected() {
                self.core.reset_reader();
                self.connection = None;
            }

            if self.connection.is_none() {
                let first_connect = !self.core.session_present();
                let connection = self.connector.connect(self.core.broker()).await?;
                self.connection = Some(connection);
                let mut connection = self.take_connection()?;
                let result = self.core.connect(&mut connection).await;
                self.connection = Some(connection);
                result?;
                activated = Some(first_connect);
                self.pending_activation = activated;
            }

            if self.core.is_connected() {
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
