use crate::{Broker, OwnedWill, ProtocolError, Will, types::Auth};
use embassy_time::Duration;
use heapless::String;

#[derive(Debug)]
pub struct Buffers<'a> {
    pub rx: &'a mut [u8],
    pub tx: &'a mut [u8],
    pub inflight: &'a mut [u8],
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BufferLayout {
    pub rx: usize,
    pub tx: usize,
    pub inflight: usize,
}

impl BufferLayout {
    pub fn split<'a>(self, buffer: &'a mut [u8]) -> Result<Buffers<'a>, ConfigError> {
        let total = self.rx + self.tx + self.inflight;
        if total > buffer.len() {
            return Err(ConfigError::BufferLayout);
        }

        let (rx, tail) = buffer.split_at_mut(self.rx);
        let (tx, tail) = tail.split_at_mut(self.tx);
        let (inflight, _) = tail.split_at_mut(self.inflight);
        Ok(Buffers { rx, tx, inflight })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ConfigError {
    BufferLayout,
}

#[derive(Debug)]
pub struct Config<'a> {
    pub(crate) broker: Broker<'a>,
    pub(crate) buffers: Buffers<'a>,
    pub(crate) will: Option<Will<'a>>,
    pub(crate) owned_will: Option<OwnedWill<'a>>,
    pub(crate) client_id: String<64>,
    pub(crate) keepalive_interval: Duration,
    pub(crate) downgrade_qos: bool,
    pub(crate) auth: Option<Auth<'a>>,
}

#[derive(Debug)]
pub struct ConfigBuilder<'a> {
    broker: Broker<'a>,
    buffers: Buffers<'a>,
    will: Option<Will<'a>>,
    owned_will: Option<OwnedWill<'a>>,
    client_id: String<64>,
    keepalive_interval: Duration,
    downgrade_qos: bool,
    auth: Option<Auth<'a>>,
}

impl<'a> ConfigBuilder<'a> {
    pub fn new(broker: Broker<'a>, buffers: Buffers<'a>) -> Self {
        Self {
            broker,
            buffers,
            will: None,
            owned_will: None,
            client_id: String::new(),
            auth: None,
            keepalive_interval: Duration::from_secs(59),
            downgrade_qos: false,
        }
    }

    pub fn from_buffer_layout(
        broker: Broker<'a>,
        buffer: &'a mut [u8],
        layout: BufferLayout,
    ) -> Result<Self, ConfigError> {
        Ok(Self::new(broker, layout.split(buffer)?))
    }

    #[cfg(feature = "unsecure")]
    pub fn set_auth(
        mut self,
        user_name: &'a str,
        password: &'a str,
    ) -> Result<Self, ProtocolError> {
        if self.auth.is_some() {
            return Err(ProtocolError::AuthAlreadySpecified);
        }

        self.auth.replace(Auth {
            user_name,
            password,
        });
        Ok(self)
    }

    pub fn client_id(mut self, id: &str) -> Result<Self, ProtocolError> {
        self.client_id =
            String::try_from(id).map_err(|_| ProtocolError::ProvidedClientIdTooLong)?;
        Ok(self)
    }

    pub fn keepalive_interval(mut self, seconds: u16) -> Self {
        self.keepalive_interval = Duration::from_secs(seconds as u64);
        self
    }

    pub fn autodowngrade_qos(mut self) -> Self {
        self.downgrade_qos = true;
        self
    }

    pub fn will(mut self, will: Will<'a>) -> Result<Self, ProtocolError> {
        if self.will.is_some() {
            return Err(ProtocolError::WillAlreadySpecified);
        }
        self.will = Some(will);
        Ok(self)
    }

    pub fn owned_will(mut self, will: OwnedWill<'a>) -> Result<Self, ProtocolError> {
        if self.will.is_some() || self.owned_will.is_some() {
            return Err(ProtocolError::WillAlreadySpecified);
        }
        self.owned_will = Some(will);
        Ok(self)
    }

    pub fn build(self) -> Config<'a> {
        Config {
            broker: self.broker,
            buffers: self.buffers,
            will: self.will,
            owned_will: self.owned_will,
            client_id: self.client_id,
            keepalive_interval: self.keepalive_interval,
            downgrade_qos: self.downgrade_qos,
            auth: self.auth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::IpAddr;

    #[test]
    fn split_layout() {
        let mut buffer = [0; 30];
        let layout = BufferLayout {
            rx: 10,
            tx: 12,
            inflight: 8,
        };
        let buffers = layout.split(&mut buffer).unwrap();
        assert_eq!(buffers.rx.len(), 10);
        assert_eq!(buffers.tx.len(), 12);
        assert_eq!(buffers.inflight.len(), 8);
    }

    #[test]
    fn layout_too_large() {
        let mut buffer = [0; 30];
        let layout = BufferLayout {
            rx: 10,
            tx: 12,
            inflight: 9,
        };
        assert!(matches!(
            layout.split(&mut buffer),
            Err(ConfigError::BufferLayout)
        ));
    }

    #[test]
    fn builder() {
        let mut rx = [0; 10];
        let mut tx = [0; 12];
        let mut inflight = [0; 8];
        let broker = Broker::from("127.0.0.1".parse::<IpAddr>().unwrap());
        let config = ConfigBuilder::new(
            broker,
            Buffers {
                rx: &mut rx,
                tx: &mut tx,
                inflight: &mut inflight,
            },
        )
        .build();
        assert_eq!(config.buffers.rx.len(), 10);
        assert_eq!(config.buffers.tx.len(), 12);
        assert_eq!(config.buffers.inflight.len(), 8);
    }

    #[test]
    fn will_does_not_consume_inflight_buffer() {
        let mut rx = [0; 10];
        let mut tx = [0; 12];
        let mut inflight = [0; 8];
        let broker = Broker::from("127.0.0.1".parse::<IpAddr>().unwrap());
        let will = Will::new("topic", b"x", &[]).unwrap();
        let config = ConfigBuilder::new(
            broker,
            Buffers {
                rx: &mut rx,
                tx: &mut tx,
                inflight: &mut inflight,
            },
        )
        .will(will)
        .unwrap()
        .build();
        assert_eq!(config.buffers.inflight.len(), 8);
    }
}
