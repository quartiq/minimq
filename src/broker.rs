use crate::MQTT_INSECURE_DEFAULT_PORT;
use core::{
    convert::TryFrom,
    net::{IpAddr, SocketAddr},
};
use heapless::String;

const HOST_CAPACITY: usize = 253;

/// MQTT broker endpoint configuration.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Broker {
    SocketAddr(SocketAddr),
    Hostname {
        host: String<HOST_CAPACITY>,
        port: u16,
    },
}

impl Broker {
    pub fn hostname(host: &str, port: u16) -> Result<Self, &'static str> {
        Ok(Self::Hostname {
            host: String::try_from(host).map_err(|_| "Broker hostname too long")?,
            port,
        })
    }

    pub fn host(host: &str) -> Result<Self, &'static str> {
        Self::hostname(host, MQTT_INSECURE_DEFAULT_PORT)
    }

    pub fn socket_addr(addr: SocketAddr) -> Self {
        Self::SocketAddr(addr)
    }

    pub fn port(&self) -> u16 {
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

    pub fn host_str(&self) -> Option<&str> {
        match self {
            Self::Hostname { host, .. } => Some(host.as_str()),
            Self::SocketAddr(_) => None,
        }
    }
}

impl From<IpAddr> for Broker {
    fn from(addr: IpAddr) -> Self {
        Self::SocketAddr(SocketAddr::new(addr, MQTT_INSECURE_DEFAULT_PORT))
    }
}

impl From<SocketAddr> for Broker {
    fn from(addr: SocketAddr) -> Self {
        Self::SocketAddr(addr)
    }
}
