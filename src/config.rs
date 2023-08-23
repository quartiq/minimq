use crate::{ProtocolError, Will};
use core::convert::TryFrom;
use embedded_time::duration::{Extensions, Milliseconds};
use heapless::String;

/// Configuration specifying the operational state of the MQTT client.
pub struct Config<'a> {
    pub(crate) rx_buffer: &'a mut [u8],
    pub(crate) tx_buffer: &'a mut [u8],
    pub(crate) state_buffer: &'a mut [u8],
    pub(crate) client_id: String<64>,
    pub(crate) keepalive_interval: Milliseconds<u32>,
    pub(crate) downgrade_qos: bool,
    pub(crate) will: Option<Will<'a>>,
}

impl<'a> Config<'a> {
    /// Construct configuration for the MQTT client.
    ///
    /// # Args
    /// * `rx` - Memory used for receiving messages. The length of this buffer is the maximum
    /// receive packet length.
    /// * `tx` - Memory used for transmitting messages. The length of this buffer is the max
    /// transmit length.
    pub fn new(rx: &'a mut [u8], tx: &'a mut [u8]) -> Self {
        Self {
            rx_buffer: rx,
            tx_buffer: tx,
            state_buffer: &mut [],
            client_id: String::new(),
            keepalive_interval: 59_000.milliseconds(),
            downgrade_qos: false,
            will: None,
        }
    }

    /// Provide additional buffer space if messages above [QoS::AtMostOnce] are required.
    pub fn session_state(mut self, buffer: &'a mut [u8]) -> Self {
        self.state_buffer = buffer;
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

    /// Specify if publication [QoS] should be automatically downgraded to the maximum supported by
    /// the server if they exceed the server [QoS] maximum.
    pub fn autodowngrade_qos(mut self) -> Self {
        self.downgrade_qos = true;
        self
    }

    /// Specify the Will message to be sent if the client disconnects.
    ///
    /// # Args
    /// * `will` - The will to use.
    pub fn will(mut self, will: Will<'a>) -> Self {
        self.will.replace(will);
        self
    }
}
