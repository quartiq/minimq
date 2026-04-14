use crate::{Broker, OwnedWill, ProtocolError, Will, types::Auth, will::WillSpec};
use embassy_time::Duration;
use heapless::String;

/// Caller-owned packet buffers.
///
/// `rx` holds exactly one inbound MQTT control packet at a time. Size it for the largest packet
/// you expect to receive from the broker, including topic, properties, and payload.
///
/// `tx` is the transmit arena. `minimq` uses it to:
/// - encode outbound packets before writing them
/// - retain unacknowledged QoS 1/2 publishes
/// - retain in-flight `SUBSCRIBE` and `UNSUBSCRIBE` packets until acknowledged
/// - replay retained packets after a resumed session reconnect
///
/// `tx` therefore needs to cover both the largest outbound packet and the amount of in-flight
/// state you want to allow.
#[derive(Debug)]
pub struct Buffers<'a> {
    /// Inbound packet storage.
    pub rx: &'a mut [u8],
    /// Outbound encode and retransmit storage.
    pub tx: &'a mut [u8],
}

/// Lengths for [`Buffers`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BufferLayout {
    /// Length of [`Buffers::rx`].
    pub rx: usize,
    /// Length of [`Buffers::tx`].
    pub tx: usize,
}

impl BufferLayout {
    /// Split one backing buffer into explicit RX and TX regions.
    ///
    /// `rx` is the maximum inbound packet size. `tx` is the shared outbound arena for encoding,
    /// retained QoS packets, and replay state.
    ///
    /// ```rust
    /// use minimq::BufferLayout;
    ///
    /// let mut storage = [0u8; 16];
    /// let buffers = BufferLayout {
    ///     rx: 4,
    ///     tx: 12,
    /// }
    /// .split(&mut storage)
    /// .unwrap();
    ///
    /// assert_eq!(buffers.rx.len(), 4);
    /// assert_eq!(buffers.tx.len(), 12);
    /// ```
    pub fn split<'a>(self, buffer: &'a mut [u8]) -> Result<Buffers<'a>, ConfigError> {
        let total = self.rx + self.tx;
        if total > buffer.len() {
            return Err(ConfigError::BufferLayout);
        }

        let (rx, tail) = buffer.split_at_mut(self.rx);
        let (tx, _) = tail.split_at_mut(self.tx);
        Ok(Buffers { rx, tx })
    }
}

/// Configuration errors detected before a session is built.
#[derive(Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("buffer layout exceeds backing storage")]
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
    /// Construct a session configuration from explicit packet buffers.
    ///
    /// The default session expiry is `u32::MAX`, which requests a long-lived persistent session.
    /// Call [`session_expiry_interval`](Self::session_expiry_interval) to choose a shorter-lived
    /// session or `0` for a clean session that expires on disconnect.
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

    /// Construct a session configuration by splitting one backing buffer into RX and TX regions.
    pub fn from_buffer_layout(
        broker: Broker<'a>,
        buffer: &'a mut [u8],
        layout: BufferLayout,
    ) -> Result<Self, ConfigError> {
        Ok(Self::new(broker, layout.split(buffer)?))
    }

    /// Attach MQTT username/password authentication.
    pub fn set_auth(
        mut self,
        user_name: &'a str,
        password: &'a [u8],
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

    /// Set the MQTT client identifier.
    pub fn client_id(mut self, id: &str) -> Result<Self, ProtocolError> {
        self.client_id =
            String::try_from(id).map_err(|_| ProtocolError::ProvidedClientIdTooLong)?;
        Ok(self)
    }

    /// Set the keepalive interval advertised in `CONNECT`.
    pub fn keepalive_interval(mut self, seconds: u16) -> Self {
        self.keepalive_interval = Duration::from_secs(seconds as u64);
        self
    }

    /// Set the MQTT v5 session expiry interval, in seconds.
    ///
    /// The default is `u32::MAX`, which requests a long-lived persistent session.
    pub fn session_expiry_interval(mut self, seconds: u32) -> Self {
        self.session_expiry_interval = seconds;
        self
    }

    /// Downgrade outbound publish QoS to the broker-advertised maximum QoS.
    pub fn autodowngrade_qos(mut self) -> Self {
        self.downgrade_qos = true;
        self
    }

    /// Attach a borrowed MQTT will message.
    pub fn will(mut self, will: Will<'a>) -> Result<Self, ProtocolError> {
        if self.will.is_some() {
            return Err(ProtocolError::WillAlreadySpecified);
        }
        self.will = Some(WillSpec::Borrowed(will));
        Ok(self)
    }

    /// Attach an owned MQTT will message.
    pub fn owned_will(mut self, will: OwnedWill<'a>) -> Result<Self, ProtocolError> {
        if self.will.is_some() {
            return Err(ProtocolError::WillAlreadySpecified);
        }
        self.will = Some(WillSpec::Owned(will));
        Ok(self)
    }

    /// Finalize the session configuration.
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
        let layout = BufferLayout { rx: 10, tx: 20 };
        let buffers = layout.split(&mut buffer).unwrap();
        assert_eq!(buffers.rx.len(), 10);
        assert_eq!(buffers.tx.len(), 20);
    }

    #[test]
    fn layout_too_large() {
        let mut buffer = [0; 30];
        let layout = BufferLayout { rx: 10, tx: 21 };
        assert!(matches!(
            layout.split(&mut buffer),
            Err(ConfigError::BufferLayout)
        ));
    }

    #[test]
    fn builder() {
        let mut rx = [0; 10];
        let mut tx = [0; 20];
        let broker: Broker<'_> = "127.0.0.1:1883".parse::<SocketAddr>().unwrap().into();
        let config = ConfigBuilder::new(
            broker,
            Buffers {
                rx: &mut rx,
                tx: &mut tx,
            },
        )
        .build();
        assert_eq!(config.buffers.rx.len(), 10);
        assert_eq!(config.buffers.tx.len(), 20);
    }

    #[test]
    fn will_does_not_consume_tx_buffer() {
        let mut rx = [0; 10];
        let mut tx = [0; 20];
        let broker: Broker<'_> = "127.0.0.1:1883".parse::<SocketAddr>().unwrap().into();
        let will = Will::new("topic", b"x", &[]).unwrap();
        let config = ConfigBuilder::new(
            broker,
            Buffers {
                rx: &mut rx,
                tx: &mut tx,
            },
        )
        .will(will)
        .unwrap()
        .build();
        assert_eq!(config.buffers.tx.len(), 20);
    }
}
