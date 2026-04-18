use crate::Broker;
use core::net::SocketAddr;
use embedded_io_async::ErrorKind;

/// Transport error categories exposed by `minimq`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError<E> {
    /// The requested broker endpoint is not supported by the selected connector.
    #[error("unsupported broker endpoint")]
    UnsupportedBroker,
    /// Hostname resolution failed before a TCP connection could be opened.
    #[error("dns resolution failed")]
    Dns,
    /// Opening the underlying connection failed.
    #[error("connect error: {0:?}")]
    Connect(E),
    /// I/O on an established connection failed.
    #[error("i/o error: {0:?}")]
    Io(E),
}

impl<E> TransportError<E> {
    /// Return the wrapped transport error, if any.
    pub const fn error(&self) -> Option<&E> {
        match self {
            Self::Connect(error) | Self::Io(error) => Some(error),
            Self::UnsupportedBroker | Self::Dns => None,
        }
    }
}

impl<E> embedded_io_async::Error for TransportError<E>
where
    E: embedded_io_async::Error,
{
    fn kind(&self) -> ErrorKind {
        match self {
            Self::UnsupportedBroker => ErrorKind::Unsupported,
            Self::Dns => ErrorKind::Other,
            Self::Connect(error) | Self::Io(error) => error.kind(),
        }
    }
}

/// Local address-family selection for hostname resolution.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AddrType {
    /// Resolve IPv4 addresses only.
    IPv4,
    /// Resolve IPv6 addresses only.
    IPv6,
    /// Let the resolver choose either family.
    Either,
}

impl From<AddrType> for embedded_nal_async::AddrType {
    fn from(value: AddrType) -> Self {
        match value {
            AddrType::IPv4 => Self::IPv4,
            AddrType::IPv6 => Self::IPv6,
            AddrType::Either => Self::Either,
        }
    }
}

#[allow(async_fn_in_trait)]
/// Factory for broker connections used by [`Session`](crate::Session).
///
/// Implement this for your transport stack or use [`TcpConnector`] / [`DnsTcpConnector`].
pub trait Connector {
    /// Transport error type used for connect and stream I/O.
    type Error: embedded_io_async::Error;
    /// Connected byte stream used by the session.
    type Connection<'a>: embedded_io_async::Read<Error = Self::Error>
        + embedded_io_async::Write<Error = Self::Error>
        + 'a
    where
        Self: 'a;

    /// Connect to `broker`.
    async fn connect<'a>(
        &'a self,
        broker: &Broker<'_>,
    ) -> Result<Self::Connection<'a>, Self::Error>;
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

    async fn connect<'a>(
        &'a self,
        broker: &Broker<'_>,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        (*self).connect(broker).await
    }
}

/// Connector for stacks that can open TCP connections to concrete socket addresses.
#[derive(Debug, Copy, Clone)]
pub struct TcpConnector<T> {
    /// Underlying TCP stack.
    stack: T,
}

impl<T> TcpConnector<T> {
    /// Wrap a TCP stack as a [`Connector`].
    pub const fn new(stack: T) -> Self {
        Self { stack }
    }

    /// Borrow the wrapped TCP stack.
    pub const fn stack(&self) -> &T {
        &self.stack
    }

    /// Consume the connector and return the wrapped TCP stack.
    pub fn into_inner(self) -> T {
        self.stack
    }
}

impl<T> Connector for TcpConnector<T>
where
    T: embedded_nal_async::TcpConnect,
    T::Error: embedded_io_async::Error,
{
    type Error = TransportError<T::Error>;
    type Connection<'a>
        = TcpConnection<T::Connection<'a>>
    where
        Self: 'a;

    async fn connect<'a>(
        &'a self,
        broker: &Broker<'_>,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        let Broker::SocketAddr(addr) = broker else {
            return Err(TransportError::UnsupportedBroker);
        };
        self.stack
            .connect(*addr)
            .await
            .map(TcpConnection)
            .map_err(TransportError::Connect)
    }
}

/// Connector for stacks that need separate DNS resolution before opening TCP.
#[derive(Debug, Clone)]
pub struct DnsTcpConnector<T, D> {
    /// Underlying TCP stack.
    stack: T,
    /// DNS resolver.
    dns: D,
    /// Requested address family for hostname lookups.
    addr_type: AddrType,
}

impl<T, D> DnsTcpConnector<T, D> {
    /// Wrap a TCP stack and DNS resolver as a [`Connector`].
    pub const fn new(stack: T, dns: D, addr_type: AddrType) -> Self {
        Self {
            stack,
            dns,
            addr_type,
        }
    }

    /// Borrow the wrapped TCP stack.
    pub const fn stack(&self) -> &T {
        &self.stack
    }

    /// Borrow the wrapped DNS resolver.
    pub const fn dns(&self) -> &D {
        &self.dns
    }

    /// Return the configured address-family preference.
    pub const fn addr_type(&self) -> AddrType {
        self.addr_type
    }

    /// Consume the connector and return the wrapped pieces.
    pub fn into_inner(self) -> (T, D, AddrType) {
        (self.stack, self.dns, self.addr_type)
    }
}

impl<T, D> Connector for DnsTcpConnector<T, D>
where
    T: embedded_nal_async::TcpConnect,
    T::Error: embedded_io_async::Error,
    D: embedded_nal_async::Dns,
{
    type Error = TransportError<T::Error>;
    type Connection<'a>
        = TcpConnection<T::Connection<'a>>
    where
        Self: 'a;

    async fn connect<'a>(
        &'a self,
        broker: &Broker<'_>,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        let addr = match broker {
            Broker::SocketAddr(addr) => *addr,
            Broker::Hostname { host, port } => {
                let ip = self
                    .dns
                    .get_host_by_name(host, self.addr_type.into())
                    .await
                    .map_err(|_| TransportError::Dns)?;
                SocketAddr::new(ip, *port)
            }
        };

        self.stack
            .connect(addr)
            .await
            .map(TcpConnection)
            .map_err(TransportError::Connect)
    }
}

#[derive(Debug)]
#[doc(hidden)]
pub struct TcpConnection<C>(C);

impl<C> embedded_io_async::ErrorType for TcpConnection<C>
where
    C: embedded_io_async::ErrorType,
    C::Error: embedded_io_async::Error,
{
    type Error = TransportError<C::Error>;
}

impl<C> embedded_io_async::Read for TcpConnection<C>
where
    C: embedded_io_async::Read,
    C::Error: embedded_io_async::Error,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buf).await.map_err(TransportError::Io)
    }
}

impl<C> embedded_io_async::Write for TcpConnection<C>
where
    C: embedded_io_async::Write,
    C::Error: embedded_io_async::Error,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.write(buf).await.map_err(TransportError::Io)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await.map_err(TransportError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MQTT_INSECURE_DEFAULT_PORT;
    use embedded_io_async::{ErrorType, Read, Write};
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

    impl embedded_nal_async::TcpConnect for NeverStack {
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

    impl embedded_nal_async::Dns for NeverDns {
        type Error = ErrorKind;

        async fn get_host_by_name(
            &self,
            _host: &str,
            _addr_type: embedded_nal_async::AddrType,
        ) -> Result<IpAddr, Self::Error> {
            Err(ErrorKind::Other)
        }

        async fn get_host_by_address(
            &self,
            _addr: IpAddr,
            _result: &mut [u8],
        ) -> Result<usize, Self::Error> {
            Err(ErrorKind::Other)
        }
    }

    #[test]
    fn hostname_requires_dns_error() {
        let broker = Broker::hostname("broker", MQTT_INSECURE_DEFAULT_PORT);
        let connector = TcpConnector::new(NeverStack);
        let result = crate::tests::block_on(async { connector.connect(&broker).await });
        match result {
            Err(TransportError::<ErrorKind>::UnsupportedBroker) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn dns_connector_maps_dns_errors() {
        let broker = Broker::hostname("broker", MQTT_INSECURE_DEFAULT_PORT);
        let connector = DnsTcpConnector::new(NeverStack, NeverDns, AddrType::IPv4);
        let result = crate::tests::block_on(async { connector.connect(&broker).await });
        match result {
            Err(TransportError::<ErrorKind>::Dns) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
