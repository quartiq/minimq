use crate::{warn, MQTT_INSECURE_DEFAULT_PORT};
use core::{
    convert::TryFrom,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use embedded_nal::{nb, AddrType, Dns};

/// A type that allows us to (eventually) determine the broker address.
pub trait Broker {
    /// Retrieve the broker address (if available).
    fn get_address(&mut self) -> Option<SocketAddr>;

    /// Set the port of the broker connection.
    fn set_port(&mut self, port: u16);
}

/// A broker that is specified using a qualified domain-name. The name will be resolved at some
/// point in the future.
#[derive(Debug)]
pub struct NamedBroker<R, const T: usize = 253> {
    raw: heapless::String<T>,
    resolver: R,
    addr: SocketAddr,
}

impl<R, const T: usize> NamedBroker<R, T> {
    /// Construct a new named broker.
    ///
    /// # Args
    /// * `broker` - The domain name of the broker, such as `broker.example.com`
    /// * `resolver` - A [embedded_nal::Dns] resolver to resolve the broker domain name to an IP
    ///   address.
    pub fn new(broker: &str, resolver: R) -> Result<Self, &str> {
        let addr: Ipv4Addr = broker.parse().unwrap_or(Ipv4Addr::UNSPECIFIED);

        Ok(Self {
            raw: heapless::String::try_from(broker).map_err(|_| "Broker domain name too long")?,
            resolver,
            addr: SocketAddr::new(IpAddr::V4(addr), MQTT_INSECURE_DEFAULT_PORT),
        })
    }
}

impl<R: Dns, const T: usize> Broker for NamedBroker<R, T> {
    fn get_address(&mut self) -> Option<SocketAddr> {
        // Attempt to resolve the address.
        if self.addr.ip().is_unspecified() {
            match self.resolver.get_host_by_name(&self.raw, AddrType::IPv4) {
                Ok(ip) => self.addr.set_ip(ip),
                Err(nb::Error::WouldBlock) => {}
                Err(_other) => {
                    warn!("DNS lookup failed: {_other:?}")
                }
            }
        }

        if !self.addr.ip().is_unspecified() {
            Some(self.addr)
        } else {
            None
        }
    }

    fn set_port(&mut self, port: u16) {
        self.addr.set_port(port)
    }
}

/// A simple broker specification where the address of the broker is known in advance.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct IpBroker {
    addr: SocketAddr,
}

impl IpBroker {
    /// Construct a broker with a known IP address.
    pub fn new(broker: IpAddr) -> Self {
        Self {
            addr: SocketAddr::new(broker, MQTT_INSECURE_DEFAULT_PORT),
        }
    }
}
impl Broker for IpBroker {
    fn get_address(&mut self) -> Option<SocketAddr> {
        Some(self.addr)
    }

    fn set_port(&mut self, port: u16) {
        self.addr.set_port(port)
    }
}

impl From<IpAddr> for IpBroker {
    fn from(addr: IpAddr) -> Self {
        IpBroker::new(addr)
    }
}
