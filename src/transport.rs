use crate::Broker;
use core::net::SocketAddr;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError<E> {
    Transport(E),
    HostnameRequiresDns,
}

#[allow(async_fn_in_trait)]
pub trait Connector {
    type ConnectError;
    type IoError;
    type Connection<'a>: Read + Write + ErrorType<Error = Self::IoError> + 'a
    where
        Self: 'a;

    async fn connect<'a, const N: usize>(
        &'a self,
        broker: &Broker<N>,
    ) -> Result<Self::Connection<'a>, Self::ConnectError>;
}

pub struct TcpConnector<T> {
    stack: T,
}

impl<T> TcpConnector<T> {
    pub fn new(stack: T) -> Self {
        Self { stack }
    }
}

impl<T> Connector for TcpConnector<T>
where
    T: TcpConnect,
{
    type ConnectError = ConnectError<T::Error>;
    type IoError = T::Error;
    type Connection<'a>
        = T::Connection<'a>
    where
        Self: 'a;

    async fn connect<'a, const N: usize>(
        &'a self,
        broker: &Broker<N>,
    ) -> Result<Self::Connection<'a>, Self::ConnectError> {
        let Broker::SocketAddr(addr) = broker else {
            return Err(ConnectError::HostnameRequiresDns);
        };
        self.stack
            .connect(*addr)
            .await
            .map_err(ConnectError::Transport)
    }
}

pub struct DnsTcpConnector<T, D> {
    stack: T,
    dns: D,
}

impl<T, D> DnsTcpConnector<T, D> {
    pub fn new(stack: T, dns: D) -> Self {
        Self { stack, dns }
    }
}

impl<T, D> Connector for DnsTcpConnector<T, D>
where
    T: TcpConnect,
    D: Dns<Error = T::Error>,
{
    type ConnectError = ConnectError<T::Error>;
    type IoError = T::Error;
    type Connection<'a>
        = T::Connection<'a>
    where
        Self: 'a;

    async fn connect<'a, const N: usize>(
        &'a self,
        broker: &Broker<N>,
    ) -> Result<Self::Connection<'a>, Self::ConnectError> {
        let addr = match broker {
            Broker::SocketAddr(addr) => *addr,
            Broker::Hostname { host, port } => {
                let ip = self
                    .dns
                    .get_host_by_name(host.as_str(), AddrType::Either)
                    .await
                    .map_err(ConnectError::Transport)?;
                SocketAddr::new(ip, *port)
            }
        };
        self.stack
            .connect(addr)
            .await
            .map_err(ConnectError::Transport)
    }
}

pub enum Either<L, R> {
    Left(L),
    Right(R),
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::net::SocketAddr;
    use embedded_io_async::{ErrorKind, ErrorType, Read, Write};

    #[derive(Debug)]
    struct DummyConnection;

    impl ErrorType for DummyConnection {
        type Error = ErrorKind;
    }

    impl Read for DummyConnection {
        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
            Ok(0)
        }
    }

    impl Write for DummyConnection {
        async fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
            Ok(0)
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct DummyStack;

    impl TcpConnect for DummyStack {
        type Error = ErrorKind;
        type Connection<'a> = DummyConnection;

        async fn connect<'a>(
            &'a self,
            _remote: SocketAddr,
        ) -> Result<Self::Connection<'a>, Self::Error> {
            Ok(DummyConnection)
        }
    }

    fn block_on<F: core::future::Future>(future: F) -> F::Output {
        use core::task::{Context, Poll, Waker};
        use std::sync::Arc;
        struct Noop;
        impl std::task::Wake for Noop {
            fn wake(self: Arc<Self>) {}
        }
        let waker = Waker::from(Arc::new(Noop));
        let mut cx = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn hostname_requires_dns_error() {
        let connector = TcpConnector::new(DummyStack);
        let broker: Broker = Broker::host("broker.example").unwrap();
        let result = block_on(connector.connect(&broker));
        assert!(matches!(result, Err(ConnectError::HostnameRequiresDns)));
    }
}
