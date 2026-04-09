use crate::MQTT_INSECURE_DEFAULT_PORT;
use core::net::{IpAddr, SocketAddr};

/// MQTT broker endpoint configuration.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Broker<'a> {
    SocketAddr(SocketAddr),
    Hostname { host: &'a str, port: u16 },
}

impl<'a> Broker<'a> {
    pub const fn hostname(host: &'a str, port: u16) -> Self {
        Self::Hostname { host, port }
    }

    pub const fn host(host: &'a str) -> Self {
        Self::hostname(host, MQTT_INSECURE_DEFAULT_PORT)
    }

    pub const fn socket_addr(addr: SocketAddr) -> Self {
        Self::SocketAddr(addr)
    }

    pub const fn port(&self) -> u16 {
        match self {
            Self::SocketAddr(addr) => addr.port(),
            Self::Hostname { port, .. } => *port,
        }
    }

    pub fn set_port(&mut self, port: u16) {
        match self {
            Self::SocketAddr(addr) => addr.set_port(port),
            Self::Hostname { port: value, .. } => *value = port,
        }
    }

    pub const fn host_str(&self) -> Option<&str> {
        match self {
            Self::Hostname { host, .. } => Some(host),
            Self::SocketAddr(_) => None,
        }
    }
}

impl From<IpAddr> for Broker<'_> {
    fn from(addr: IpAddr) -> Self {
        Self::SocketAddr(SocketAddr::new(addr, MQTT_INSECURE_DEFAULT_PORT))
    }
}

impl From<SocketAddr> for Broker<'_> {
    fn from(addr: SocketAddr) -> Self {
        Self::SocketAddr(addr)
    }
}
