use embedded_nal::{nb, TcpClientStack};
use std::cell::RefCell;
use std::io::{Error, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TcpHandle {
    id: usize,
}

pub struct MitmStack<'a> {
    pub sockets: &'a RefCell<Vec<(TcpHandle, TcpSocket)>>,
    handle: usize,
}

impl<'a> MitmStack<'a> {
    pub fn new(sockets: &'a RefCell<Vec<(TcpHandle, TcpSocket)>>) -> Self {
        Self { sockets, handle: 0 }
    }

    pub fn add_socket(&mut self) -> TcpHandle {
        let handle = TcpHandle { id: self.handle };
        self.handle += 1;
        self.sockets.borrow_mut().push((handle, TcpSocket::new()));
        handle
    }

    pub fn close(&mut self, handle: TcpHandle) {
        let index = self
            .sockets
            .borrow_mut()
            .iter()
            .position(|(h, _sock)| h == &handle)
            .unwrap();
        self.sockets.borrow_mut().swap_remove(index);
    }
}

#[derive(Debug)]
pub struct TcpError(pub Error);

impl TcpError {
    fn broken_pipe() -> TcpError {
        TcpError(Error::new(ErrorKind::BrokenPipe, "Connection interrupted"))
    }
}

fn to_nb(e: std::io::Error) -> nb::Error<TcpError> {
    match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => nb::Error::WouldBlock,
        _ => nb::Error::Other(TcpError(e)),
    }
}

impl From<Error> for TcpError {
    fn from(e: Error) -> Self {
        Self(e)
    }
}

impl embedded_nal::TcpError for TcpError {
    fn kind(&self) -> embedded_nal::TcpErrorKind {
        match self.0.kind() {
            std::io::ErrorKind::BrokenPipe => embedded_nal::TcpErrorKind::PipeClosed,
            _ => embedded_nal::TcpErrorKind::Other,
        }
    }
}

pub struct TcpSocket {
    stream: Option<TcpStream>,
}

impl TcpSocket {
    fn new() -> Self {
        Self { stream: None }
    }

    fn connect(&mut self, remote: embedded_nal::SocketAddr) -> Result<(), Error> {
        let embedded_nal::IpAddr::V4(addr) = remote.ip() else {
            return Err(Error::new(ErrorKind::Other, "Only IPv4 supported"));
        };
        let remote = SocketAddr::new(addr.octets().into(), remote.port());
        let soc = TcpStream::connect(remote)?;
        soc.set_nonblocking(true)?;
        self.stream.replace(soc);
        Ok(())
    }

    fn stream_mut(&mut self) -> Result<&mut TcpStream, nb::Error<TcpError>> {
        self.stream
            .as_mut()
            .ok_or_else(|| nb::Error::Other(TcpError::broken_pipe()))
    }

    pub fn close(&mut self) {
        self.stream.take();
    }
}

impl<'a> TcpClientStack for MitmStack<'a> {
    type TcpSocket = TcpHandle;
    type Error = TcpError;

    fn socket(&mut self) -> Result<Self::TcpSocket, Self::Error> {
        Ok(self.add_socket())
    }

    fn connect(
        &mut self,
        socket: &mut Self::TcpSocket,
        remote: embedded_nal::SocketAddr,
    ) -> nb::Result<(), Self::Error> {
        let index = self
            .sockets
            .borrow_mut()
            .iter()
            .position(|(h, _sock)| h == socket)
            .unwrap();
        let socket = &mut self.sockets.borrow_mut()[index].1;
        socket.connect(remote).map_err(to_nb)?;
        Ok(())
    }

    fn send(
        &mut self,
        socket: &mut Self::TcpSocket,
        buffer: &[u8],
    ) -> nb::Result<usize, Self::Error> {
        let index = self
            .sockets
            .borrow_mut()
            .iter()
            .position(|(h, _sock)| h == socket)
            .unwrap();
        let socket = &mut self.sockets.borrow_mut()[index].1;

        let socket = socket.stream_mut()?;
        socket.write(buffer).map_err(to_nb)
    }

    fn receive(
        &mut self,
        socket: &mut Self::TcpSocket,
        buffer: &mut [u8],
    ) -> nb::Result<usize, Self::Error> {
        let index = self
            .sockets
            .borrow_mut()
            .iter()
            .position(|(h, _sock)| h == socket)
            .unwrap();
        let socket = &mut self.sockets.borrow_mut()[index].1;
        let socket = socket.stream_mut()?;
        socket.read(buffer).map_err(to_nb)
    }

    fn close(&mut self, socket: Self::TcpSocket) -> Result<(), Self::Error> {
        self.close(socket);
        Ok(())
    }
}
