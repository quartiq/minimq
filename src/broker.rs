use crate::MQTT_INSECURE_DEFAULT_PORT;
use embedded_nal::{nb, AddrType, Dns, IpAddr, Ipv4Addr, SocketAddr};

pub trait Broker {
    fn get_address(&mut self) -> Option<SocketAddr>;
    fn set_port(&mut self, port: u16);
}

pub struct NamedBroker<R: Dns> {
    raw: &'static str,
    resolver: R,
    addr: SocketAddr,
}

impl<R: Dns> NamedBroker<R> {
    pub fn new(broker: &'static str, resolver: R) -> Self {
        let addr: Ipv4Addr = broker.parse().unwrap_or(Ipv4Addr::UNSPECIFIED);

        Self {
            raw: broker,
            resolver,
            addr: SocketAddr::new(IpAddr::V4(addr), MQTT_INSECURE_DEFAULT_PORT),
        }
    }
}

impl<R: Dns> Broker for NamedBroker<R> {
    fn get_address(&mut self) -> Option<SocketAddr> {
        // Attempt to resolve the address.
        if !self.addr.ip().is_unspecified() {
            match self.resolver.get_host_by_name(self.raw, AddrType::IPv4) {
                Ok(ip) => self.addr.set_ip(ip),
                Err(nb::Error::WouldBlock) => {}
                other => {
                    other.unwrap();
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

pub struct IpBroker {
    addr: SocketAddr,
}

impl IpBroker {
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
