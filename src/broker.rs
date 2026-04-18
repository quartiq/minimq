use core::net::SocketAddr;

/// MQTT broker endpoint configuration.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Broker<'a> {
    /// Connect to a resolved socket address directly.
    SocketAddr(SocketAddr),
    /// Connect by hostname and port. This requires a connector that can resolve hostnames.
    Hostname {
        /// DNS name to resolve.
        host: &'a str,
        /// TCP port.
        port: u16,
    },
}

impl<'a> Broker<'a> {
    /// Construct a broker endpoint from a hostname and port.
    ///
    /// ```rust
    /// use minimq::Broker;
    ///
    /// let broker = Broker::hostname("broker.example", 1883);
    /// assert_eq!(broker.port(), 1883);
    /// ```
    pub const fn hostname(host: &'a str, port: u16) -> Self {
        Self::Hostname { host, port }
    }

    /// Return the configured TCP port.
    pub const fn port(&self) -> u16 {
        match self {
            Self::SocketAddr(addr) => addr.port(),
            Self::Hostname { port, .. } => *port,
        }
    }
}

impl From<SocketAddr> for Broker<'_> {
    fn from(addr: SocketAddr) -> Self {
        Self::SocketAddr(addr)
    }
}
