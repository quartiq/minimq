use crate::MQTT_INSECURE_DEFAULT_PORT;
use core::{
    convert::TryFrom,
    net::{IpAddr, SocketAddr},
};
use heapless::String;

/// MQTT broker endpoint configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Broker<const N: usize = 253> {
    SocketAddr(SocketAddr),
    Hostname { host: String<N>, port: u16 },
}

impl<const N: usize> Broker<N> {
    pub fn host(host: &str) -> Result<Self, &'static str> {
        Ok(Self::Hostname {
            host: String::try_from(host).map_err(|_| "Broker hostname too long")?,
            port: MQTT_INSECURE_DEFAULT_PORT,
        })
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
