use crate::{Broker, OwnedWill, ProtocolError, Will, types::Auth, will::WillSpec};
use embassy_time::Duration;
use heapless::String;

#[derive(Debug)]
pub struct Buffers<'a> {
    pub rx: &'a mut [u8],
    pub outbound: &'a mut [u8],
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BufferLayout {
    pub rx: usize,
    pub outbound: usize,
}

impl BufferLayout {
    /// Split one backing buffer into explicit RX and outbound regions.
    ///
    /// ```rust
    /// use minimq::BufferLayout;
    ///
    /// let mut storage = [0u8; 16];
    /// let buffers = BufferLayout {
    ///     rx: 4,
    ///     outbound: 12,
    /// }
    /// .split(&mut storage)
    /// .unwrap();
    ///
    /// assert_eq!(buffers.rx.len(), 4);
    /// assert_eq!(buffers.outbound.len(), 12);
    /// ```
    pub fn split<'a>(self, buffer: &'a mut [u8]) -> Result<Buffers<'a>, ConfigError> {
        let total = self.rx + self.outbound;
        if total > buffer.len() {
            return Err(ConfigError::BufferLayout);
        }

        let (rx, tail) = buffer.split_at_mut(self.rx);
        let (outbound, _) = tail.split_at_mut(self.outbound);
        Ok(Buffers { rx, outbound })
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
    pub(crate) will: Option<WillSpec<'a>>,
    pub(crate) client_id: String<64>,
    pub(crate) keepalive_interval: Duration,
    pub(crate) session_expiry_interval: u32,
    pub(crate) downgrade_qos: bool,
    pub(crate) auth: Option<Auth<'a>>,
}

#[derive(Debug)]
pub struct ConfigBuilder<'a> {
    broker: Broker<'a>,
    buffers: Buffers<'a>,
    will: Option<WillSpec<'a>>,
    client_id: String<64>,
    keepalive_interval: Duration,
    session_expiry_interval: u32,
    downgrade_qos: bool,
    auth: Option<Auth<'a>>,
}

impl<'a> ConfigBuilder<'a> {
    pub fn new(broker: Broker<'a>, buffers: Buffers<'a>) -> Self {
        Self {
            broker,
            buffers,
            will: None,
            client_id: String::new(),
            auth: None,
            keepalive_interval: Duration::from_secs(59),
            session_expiry_interval: u32::MAX,
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

    pub fn session_expiry_interval(mut self, seconds: u32) -> Self {
        self.session_expiry_interval = seconds;
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
        self.will = Some(WillSpec::Borrowed(will));
        Ok(self)
    }

    pub fn owned_will(mut self, will: OwnedWill<'a>) -> Result<Self, ProtocolError> {
        if self.will.is_some() {
            return Err(ProtocolError::WillAlreadySpecified);
        }
        self.will = Some(WillSpec::Owned(will));
        Ok(self)
    }

    pub fn build(self) -> Config<'a> {
        Config {
            broker: self.broker,
            buffers: self.buffers,
            will: self.will,
            client_id: self.client_id,
            keepalive_interval: self.keepalive_interval,
            session_expiry_interval: self.session_expiry_interval,
            downgrade_qos: self.downgrade_qos,
            auth: self.auth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::SocketAddr;

    #[test]
    fn split_layout() {
        let mut buffer = [0; 30];
        let layout = BufferLayout {
            rx: 10,
            outbound: 20,
        };
        let buffers = layout.split(&mut buffer).unwrap();
        assert_eq!(buffers.rx.len(), 10);
        assert_eq!(buffers.outbound.len(), 20);
    }

    #[test]
    fn layout_too_large() {
        let mut buffer = [0; 30];
        let layout = BufferLayout {
            rx: 10,
            outbound: 21,
        };
        assert!(matches!(
            layout.split(&mut buffer),
            Err(ConfigError::BufferLayout)
        ));
    }

    #[test]
    fn builder() {
        let mut rx = [0; 10];
        let mut outbound = [0; 20];
        let broker: Broker<'_> = "127.0.0.1:1883".parse::<SocketAddr>().unwrap().into();
        let config = ConfigBuilder::new(
            broker,
            Buffers {
                rx: &mut rx,
                outbound: &mut outbound,
            },
        )
        .build();
        assert_eq!(config.buffers.rx.len(), 10);
        assert_eq!(config.buffers.outbound.len(), 20);
    }

    #[test]
    fn will_does_not_consume_outbound_buffer() {
        let mut rx = [0; 10];
        let mut outbound = [0; 20];
        let broker: Broker<'_> = "127.0.0.1:1883".parse::<SocketAddr>().unwrap().into();
        let will = Will::new("topic", b"x", &[]).unwrap();
        let config = ConfigBuilder::new(
            broker,
            Buffers {
                rx: &mut rx,
                outbound: &mut outbound,
            },
        )
        .will(will)
        .unwrap()
        .build();
        assert_eq!(config.buffers.outbound.len(), 20);
    }
}
