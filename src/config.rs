use crate::{ProtocolError, Will, types::Auth};
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
    rx: &'a mut [u8],
    tx: &'a mut [u8],
}

impl<'a> Buffers<'a> {
    /// Construct caller-owned RX/TX packet buffers.
    pub const fn new(rx: &'a mut [u8], tx: &'a mut [u8]) -> Self {
        Self { rx, tx }
    }

    /// Borrow the inbound packet storage.
    pub const fn rx(&self) -> &[u8] {
        self.rx
    }

    /// Borrow the outbound packet storage.
    pub const fn tx(&self) -> &[u8] {
        self.tx
    }

    pub(crate) fn into_parts(self) -> (&'a mut [u8], &'a mut [u8]) {
        (self.rx, self.tx)
    }

    /// Split one backing buffer into explicit RX and TX regions.
    ///
    /// `rx_size` is the maximum inbound packet size. The remainder becomes the shared outbound
    /// arena for encoding, retained QoS packets, and replay state.
    ///
    /// ```rust
    /// use minimq::Buffers;
    ///
    /// let mut storage = [0u8; 16];
    /// let buffers = Buffers::split(&mut storage, 4).unwrap();
    ///
    /// assert_eq!(buffers.rx().len(), 4);
    /// assert_eq!(buffers.tx().len(), 12);
    /// ```
    pub fn split(buffer: &'a mut [u8], rx_size: usize) -> Result<Self, SetupError> {
        if rx_size > buffer.len() {
            return Err(SetupError::BufferSplit);
        }

        let (rx, tx) = buffer.split_at_mut(rx_size);
        Ok(Self::new(rx, tx))
    }
}

/// Setup errors detected before a session is created.
#[derive(Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupError {
    /// The requested RX split does not fit in the provided backing buffer.
    #[error("buffer split exceeds backing storage")]
    BufferSplit,
}

/// Builder for session setup.
///
/// The builder only covers application-facing settings. Runtime MQTT state lives in [`Session`](crate::Session).
#[derive(Debug)]
pub struct ConfigBuilder<'a> {
    buffers: Buffers<'a>,
    will: Option<Will<'a>>,
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
    pub fn new(buffers: Buffers<'a>) -> Self {
        Self {
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
    pub fn from_buffer(buffer: &'a mut [u8], rx_size: usize) -> Result<Self, SetupError> {
        Ok(Self::new(Buffers::split(buffer, rx_size)?))
    }

    /// Attach MQTT username/password authentication.
    ///
    /// Calling this more than once returns [`ProtocolError::AuthAlreadySpecified`].
    pub fn auth(mut self, user_name: &'a str, password: &'a [u8]) -> Result<Self, ProtocolError> {
        if self.auth.is_some() {
            return Err(ProtocolError::AuthAlreadySpecified);
        }

        self.auth.replace(Auth::new(user_name, password));
        Ok(self)
    }

    /// Set the MQTT client identifier.
    ///
    /// The ID must fit in the internal fixed-capacity storage.
    pub fn client_id(mut self, id: &str) -> Result<Self, ProtocolError> {
        self.client_id =
            String::try_from(id).map_err(|_| ProtocolError::ProvidedClientIdTooLong)?;
        Ok(self)
    }

    /// Set the keepalive interval advertised in `CONNECT`.
    ///
    /// `poll()` must still be driven often enough for the session to honor it.
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
    ///
    /// Without this, a publish above the broker limit fails instead.
    pub fn autodowngrade_qos(mut self) -> Self {
        self.downgrade_qos = true;
        self
    }

    /// Return the configured inbound packet buffer length.
    pub const fn rx_len(&self) -> usize {
        self.buffers.rx().len()
    }

    /// Return the configured outbound packet arena length.
    pub const fn tx_len(&self) -> usize {
        self.buffers.tx().len()
    }

    /// Attach an MQTT will message.
    pub fn will(mut self, will: Will<'a>) -> Result<Self, ProtocolError> {
        if self.will.is_some() {
            return Err(ProtocolError::WillAlreadySpecified);
        }
        self.will = Some(will);
        Ok(self)
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Buffers<'a>,
        Option<Will<'a>>,
        String<64>,
        Duration,
        u32,
        bool,
        Option<Auth<'a>>,
    ) {
        (
            self.buffers,
            self.will,
            self.client_id,
            self.keepalive_interval,
            self.session_expiry_interval,
            self.downgrade_qos,
            self.auth,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_buffer() {
        let mut buffer = [0; 30];
        let buffers = Buffers::split(&mut buffer, 10).unwrap();
        assert_eq!(buffers.rx.len(), 10);
        assert_eq!(buffers.tx.len(), 20);
    }

    #[test]
    fn split_too_large() {
        let mut buffer = [0; 30];
        assert!(matches!(
            Buffers::split(&mut buffer, 31),
            Err(SetupError::BufferSplit)
        ));
    }

    #[test]
    fn builder() {
        let mut rx = [0; 10];
        let mut tx = [0; 20];
        let (buffers, will, client_id, _, _, _, auth) = ConfigBuilder::new(Buffers {
            rx: &mut rx,
            tx: &mut tx,
        })
        .into_parts();
        assert_eq!(buffers.rx.len(), 10);
        assert_eq!(buffers.tx.len(), 20);
        assert!(will.is_none());
        assert!(client_id.is_empty());
        assert!(auth.is_none());
    }

    #[test]
    fn will_does_not_consume_tx_buffer() {
        let mut rx = [0; 10];
        let mut tx = [0; 20];
        let will = Will::new("topic", b"x", &[]).unwrap();
        let (buffers, _, _, _, _, _, _) = ConfigBuilder::new(Buffers {
            rx: &mut rx,
            tx: &mut tx,
        })
        .will(will)
        .unwrap()
        .into_parts();
        assert_eq!(buffers.tx.len(), 20);
    }
}
