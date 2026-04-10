use crate::{Broker, Error};
use core::net::SocketAddr;
use embedded_io_async::{Error as _, Read, Write};
use embedded_nal_async::AddrType;
use embedded_nal_async::{Dns, TcpConnect};

#[allow(async_fn_in_trait)]
pub trait Connector {
    type Error: embedded_io_async::Error;
    type Connection<'a>: Read<Error = Self::Error> + Write<Error = Self::Error> + 'a
    where
        Self: 'a;

    async fn connect<'a>(&'a self, broker: &Broker<'_>) -> Result<Self::Connection<'a>, Error>;
}

impl<T> Connector for &T
where
    T: Connector,
{
    type Error = T::Error;
    type Connection<'a>
        = T::Connection<'a>
    where
        Self: 'a;

    async fn connect<'a>(&'a self, broker: &Broker<'_>) -> Result<Self::Connection<'a>, Error> {
        (*self).connect(broker).await
    }
}

#[derive(Debug, Copy, Clone)]
pub struct TcpConnector<T> {
    pub stack: T,
}

impl<T> TcpConnector<T> {
    pub const fn new(stack: T) -> Self {
        Self { stack }
    }
}

impl<T> Connector for TcpConnector<T>
where
    T: TcpConnect,
{
    type Error = T::Error;
    type Connection<'a>
        = T::Connection<'a>
    where
        Self: 'a;

    async fn connect<'a>(&'a self, broker: &Broker<'_>) -> Result<Self::Connection<'a>, Error> {
        let Broker::SocketAddr(addr) = broker else {
            return Err(Error::Transport(embedded_io_async::ErrorKind::Unsupported));
        };
        self.stack
            .connect(*addr)
            .await
            .map_err(|err| Error::Transport(err.kind()))
    }
}

#[derive(Debug, Clone)]
pub struct DnsTcpConnector<T, D> {
    pub stack: T,
    pub dns: D,
    pub addr_type: AddrType,
}

impl<T, D> DnsTcpConnector<T, D> {
    pub const fn new(stack: T, dns: D, addr_type: AddrType) -> Self {
        Self {
            stack,
            dns,
            addr_type,
        }
    }
}

impl<T, D> Connector for DnsTcpConnector<T, D>
where
    T: TcpConnect,
    D: Dns,
{
    type Error = T::Error;
    type Connection<'a>
        = T::Connection<'a>
    where
        Self: 'a;

    async fn connect<'a>(&'a self, broker: &Broker<'_>) -> Result<Self::Connection<'a>, Error> {
        let addr = match broker {
            Broker::SocketAddr(addr) => *addr,
            Broker::Hostname { host, port } => {
                let ip = self
                    .dns
                    .get_host_by_name(host, self.addr_type.clone())
                    .await
                    .map_err(|_| Error::Transport(embedded_io_async::ErrorKind::Other))?;
                SocketAddr::new(ip, *port)
            }
        };

        self.stack
            .connect(addr)
            .await
            .map_err(|err| Error::Transport(err.kind()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MQTT_INSECURE_DEFAULT_PORT;
    use embedded_io_async::{ErrorKind, ErrorType};
    use embedded_nal_async::AddrType;
    use std::net::IpAddr;

    #[derive(Debug)]
    struct NeverSocket;

    impl ErrorType for NeverSocket {
        type Error = ErrorKind;
    }

    impl Read for NeverSocket {
        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
            Ok(0)
        }
    }

    impl Write for NeverSocket {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct NeverStack;

    impl TcpConnect for NeverStack {
        type Error = ErrorKind;
        type Connection<'a> = NeverSocket;

        async fn connect<'a>(
            &'a self,
            _remote: SocketAddr,
        ) -> Result<Self::Connection<'a>, Self::Error> {
            Ok(NeverSocket)
        }
    }

    struct NeverDns;

    impl Dns for NeverDns {
        type Error = ErrorKind;

        async fn get_host_by_name(
            &self,
            _host: &str,
            _addr_type: AddrType,
        ) -> Result<IpAddr, Self::Error> {
            Err(ErrorKind::Unsupported)
        }

        async fn get_host_by_address(
            &self,
            _addr: IpAddr,
            _result: &mut [u8],
        ) -> Result<usize, Self::Error> {
            Err(ErrorKind::Unsupported)
        }
    }

    #[test]
    fn hostname_requires_dns_error() {
        let broker = Broker::hostname("broker", MQTT_INSECURE_DEFAULT_PORT);
        let connector = TcpConnector::new(NeverStack);
        let result = crate::tests::block_on(async { connector.connect(&broker).await });
        assert!(matches!(
            result,
            Err(Error::Transport(ErrorKind::Unsupported))
        ));
    }

    #[test]
    fn dns_connector_maps_dns_errors() {
        let broker = Broker::hostname("broker", MQTT_INSECURE_DEFAULT_PORT);
        let connector = DnsTcpConnector::new(NeverStack, NeverDns, AddrType::IPv4);
        let result = crate::tests::block_on(async { connector.connect(&broker).await });
        assert!(matches!(result, Err(Error::Transport(ErrorKind::Other))));
    }
}
