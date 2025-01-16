use crate::{types::Auth, will::SerializedWill, ProtocolError, Will};
use core::convert::TryFrom;
use embedded_time::duration::{Extensions, Milliseconds};
use heapless::String;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum BufferConfig {
    /// Specify a buffer as having a minimum size in bytes.
    Minimum(usize),

    /// Specify a buffer as having a maximum size in bytes.
    Maximum(usize),

    /// Specify a buffer as having exactly a size in bytes.
    Exactly(usize),
}

#[derive(Debug)]
pub(crate) struct Config<'a, Broker> {
    pub(crate) broker: Broker,
    pub(crate) rx_buffer: &'a mut [u8],
    pub(crate) tx_buffer: &'a mut [u8],
    pub(crate) state_buffer: &'a mut [u8],
    pub(crate) will: Option<SerializedWill<'a>>,
    pub(crate) client_id: String<64>,
    pub(crate) keepalive_interval: Milliseconds<u32>,
    pub(crate) downgrade_qos: bool,
    pub(crate) auth: Option<Auth<'a>>,
}

/// Configuration specifying the operational state of the MQTT client.
#[derive(Debug)]
pub struct ConfigBuilder<'a, Broker> {
    buffer: &'a mut [u8],
    broker: Broker,
    rx_config: Option<BufferConfig>,
    tx_config: Option<BufferConfig>,
    session_state_config: Option<BufferConfig>,
    will: Option<SerializedWill<'a>>,
    client_id: String<64>,
    keepalive_interval: Milliseconds<u32>,
    downgrade_qos: bool,
    auth: Option<Auth<'a>>,
}

impl<'a, Broker: crate::Broker> ConfigBuilder<'a, Broker> {
    /// Construct configuration for the MQTT client.
    ///
    /// # Args
    /// * `buffer` - Memory used by the MQTT client. This memory is used for the will, the message
    /// receive buffer, the transmission buffer, and the client session state.
    pub fn new(broker: Broker, buffer: &'a mut [u8]) -> Self {
        Self {
            broker,
            buffer,
            session_state_config: None,
            rx_config: None,
            tx_config: None,
            client_id: String::new(),
            auth: None,
            keepalive_interval: 59_000.milliseconds(),
            downgrade_qos: false,
            will: None,
        }
    }

    /// Specify the authentication message used by the server.
    ///
    /// # Args
    /// * `user_name` - The user name
    /// * `password` - The password
    #[cfg(feature = "unsecure")]
    pub fn set_auth(mut self, user_name: &str, password: &str) -> Result<Self, ProtocolError> {
        if self.auth.is_some() {
            return Err(ProtocolError::AuthAlreadySpecified);
        }

        let (username_bytes, tail) = self.buffer.split_at_mut(user_name.as_bytes().len());
        username_bytes.copy_from_slice(user_name.as_bytes());
        self.buffer = tail;

        let (password_bytes, tail) = self.buffer.split_at_mut(password.as_bytes().len());
        password_bytes.copy_from_slice(password.as_bytes());
        self.buffer = tail;

        self.auth.replace(Auth {
            // Note(unwrap): We are directly copying `str` types to these buffers, so we know they
            // are valid utf8.
            user_name: core::str::from_utf8(username_bytes).unwrap(),
            password: core::str::from_utf8(password_bytes).unwrap(),
        });
        Ok(self)
    }

    /// Specify a specific configuration for the session state buffer.
    ///
    /// # Note
    /// The session state buffer is used for publications greater than [crate::QoS::AtMostOnce]. If
    /// these messages are unused, you can specify [BufferConfig::Exactly(0)].
    ///
    /// # Args
    /// * `config` - The configuration for the size of the session state buffer.
    pub fn session_state(mut self, config: BufferConfig) -> Self {
        self.session_state_config.replace(config);
        self
    }

    /// Specify a specific configuration for the message receive buffer.
    ///
    /// # Args
    /// * `config` - The configuration for the size of the receive buffer.
    pub fn rx_buffer(mut self, config: BufferConfig) -> Self {
        self.rx_config.replace(config);
        self
    }

    /// Specify a specific configuration for the message transmit buffer.
    ///
    /// # Args
    /// * `config` - The configuration for the size of the message transmit buffer.
    pub fn tx_buffer(mut self, config: BufferConfig) -> Self {
        self.tx_config.replace(config);
        self
    }

    /// Specify a known client ID to use. If not assigned, the broker will auto assign an ID.
    pub fn client_id(mut self, id: &str) -> Result<Self, ProtocolError> {
        self.client_id =
            String::try_from(id).map_err(|_| ProtocolError::ProvidedClientIdTooLong)?;
        Ok(self)
    }

    /// Configure the MQTT keep-alive interval.
    ///
    /// # Note
    /// The broker may override the requested keep-alive interval. Any value requested by the
    /// broker will be used instead.
    ///
    /// # Args
    /// * `interval` - The keep-alive interval in seconds. A ping will be transmitted if no other
    /// messages are sent within 50% of the keep-alive interval.
    pub fn keepalive_interval(mut self, seconds: u16) -> Self {
        self.keepalive_interval = Milliseconds(seconds as u32 * 1000);
        self
    }

    /// Specify if publication [crate::QoS] should be automatically downgraded to the maximum
    /// supported by the server if they exceed the server [crate::QoS] maximum.
    pub fn autodowngrade_qos(mut self) -> Self {
        self.downgrade_qos = true;
        self
    }

    /// Specify the Will message to be sent if the client disconnects.
    ///
    /// # Args
    /// * `will` - The will to use.
    pub fn will(mut self, will: Will<'_>) -> Result<Self, ProtocolError> {
        if self.will.is_some() {
            return Err(ProtocolError::WillAlreadySpecified);
        }
        let will_len = will.serialized_len();
        let (head, tail) = self.buffer.split_at_mut(will_len);
        self.buffer = tail;
        self.will = Some(will.serialize(head)?);

        Ok(self)
    }

    /// Consume the configuration and split the user-provided memory into the necessary buffers.
    pub(crate) fn build(self) -> Config<'a, Broker> {
        let configs = [self.rx_config, self.tx_config, self.session_state_config];
        let mut buffers = [None, None, None];

        let mut data = self.buffer;

        // First, check for any exact configurations.
        for (size, buffer) in configs
            .iter()
            .zip(buffers.iter_mut())
            .filter_map(|(config, buf)| {
                if let Some(BufferConfig::Exactly(size)) = config {
                    Some((size, buf))
                } else {
                    None
                }
            })
        {
            let (head, tail) = data.split_at_mut(*size);
            data = tail;
            buffer.replace(head);
        }

        // Calculate the prescribed size for remaining buffers. This will be the base size used -
        // any buffers that specify a min/max size will use this size and their respective mins or
        // maxes.
        let size = {
            let remainder = buffers.iter().filter(|x| x.is_none()).count();
            data.len().checked_div(remainder).unwrap_or(0)
        };

        // Note: We intentionally are not dynamically updating the prescribed sizes as we allocate
        // buffers because this would invalidate any previous potential min/max calculations that
        // we conducted.
        for (config, buffer) in configs
            .iter()
            .zip(buffers.iter_mut())
            .filter_map(|(config, buf)| config.as_ref().map(|config| (config, buf)))
        {
            match config {
                BufferConfig::Maximum(max) => {
                    let (head, tail) = data.split_at_mut(size.min(*max));
                    data = tail;
                    buffer.replace(head);
                }
                BufferConfig::Minimum(min) => {
                    let (head, tail) = data.split_at_mut(size.max(*min));
                    data = tail;
                    buffer.replace(head);
                }
                _ => {}
            }
        }

        // Any remaining buffers have no outstanding restrictions and the remaining memory will be
        // evenly split amongst them.
        let size = {
            let remainder = buffers.iter().filter(|x| x.is_none()).count();
            data.len().checked_div(remainder).unwrap_or(0)
        };

        for buffer in buffers.iter_mut().filter(|buf| buf.is_none()) {
            let (head, tail) = data.split_at_mut(size);
            data = tail;
            buffer.replace(head);
        }

        Config {
            broker: self.broker,
            client_id: self.client_id,
            will: self.will,
            downgrade_qos: self.downgrade_qos,
            keepalive_interval: self.keepalive_interval,
            rx_buffer: buffers[0].take().unwrap(),
            tx_buffer: buffers[1].take().unwrap(),
            state_buffer: buffers[2].take().unwrap(),
            auth: self.auth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::IpBroker;
    use core::net::IpAddr;

    #[test]
    pub fn basic_config() {
        let mut buffer = [0; 30];
        let localhost: IpAddr = "127.0.0.1".parse().unwrap();
        let builder: ConfigBuilder<IpBroker> = ConfigBuilder::new(localhost.into(), &mut buffer);

        let config = builder.build();
        assert!(config.tx_buffer.len() == 10);
        assert!(config.rx_buffer.len() == 10);
        assert!(config.state_buffer.len() == 10);
    }

    #[test]
    pub fn without_session_state() {
        let mut buffer = [0; 30];
        let localhost: IpAddr = "127.0.0.1".parse().unwrap();
        let builder: ConfigBuilder<IpBroker> = ConfigBuilder::new(localhost.into(), &mut buffer)
            .session_state(BufferConfig::Exactly(0));

        let config = builder.build();
        assert!(config.tx_buffer.len() == 15);
        assert!(config.rx_buffer.len() == 15);
        assert!(config.state_buffer.is_empty());
    }

    #[test]
    pub fn with_exact_sizes() {
        let mut buffer = [0; 30];
        let localhost: IpAddr = "127.0.0.1".parse().unwrap();
        let builder: ConfigBuilder<IpBroker> = ConfigBuilder::new(localhost.into(), &mut buffer)
            .session_state(BufferConfig::Exactly(8))
            .rx_buffer(BufferConfig::Exactly(4))
            .tx_buffer(BufferConfig::Exactly(3));

        let config = builder.build();
        assert!(config.tx_buffer.len() == 3);
        assert!(config.rx_buffer.len() == 4);
        assert!(config.state_buffer.len() == 8);
    }

    #[test]
    pub fn with_max() {
        let mut buffer = [0; 30];
        let localhost: IpAddr = "127.0.0.1".parse().unwrap();
        let builder: ConfigBuilder<IpBroker> = ConfigBuilder::new(localhost.into(), &mut buffer)
            .session_state(BufferConfig::Maximum(8));

        let config = builder.build();
        assert!(config.state_buffer.len() == 8);
        assert!(config.tx_buffer.len() == 11);
        assert!(config.rx_buffer.len() == 11);
    }

    #[test]
    pub fn with_min() {
        let mut buffer = [0; 30];
        let localhost: IpAddr = "127.0.0.1".parse().unwrap();
        let builder: ConfigBuilder<IpBroker> = ConfigBuilder::new(localhost.into(), &mut buffer)
            .session_state(BufferConfig::Minimum(20));

        let config = builder.build();
        assert!(config.state_buffer.len() == 20);
        assert!(config.tx_buffer.len() == 5);
        assert!(config.rx_buffer.len() == 5);
    }
}
