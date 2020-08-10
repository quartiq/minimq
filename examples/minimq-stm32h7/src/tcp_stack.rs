use core::cell::RefCell;
use nb;

use heapless::{consts, Vec};

use super::{net, NetworkInterface};
use minimq::embedded_nal;

#[derive(Debug)]
pub enum NetworkError {
    NoSocket,
    ConnectionFailure,
    ReadFailure,
    WriteFailure,
}

// TODO: The network stack likely needs a time-tracking mechanic here to support
// blocking/nonblocking/timeout based operations.
pub struct NetworkStack<'a, 'b, 'c, 'n> {
    network_interface: &'n mut NetworkInterface,
    sockets: RefCell<net::socket::SocketSet<'a, 'b, 'c>>,
    next_port: RefCell<u16>,
    unused_handles: RefCell<Vec<net::socket::SocketHandle, consts::U16>>,
}

impl<'a, 'b, 'c, 'n> NetworkStack<'a, 'b, 'c, 'n> {
    pub fn new(
        interface: &'n mut NetworkInterface,
        sockets: net::socket::SocketSet<'a, 'b, 'c>,
    ) -> Self {
        let mut unused_handles: Vec<net::socket::SocketHandle, consts::U16> = Vec::new();
        for socket in sockets.iter() {
            unused_handles.push(socket.handle()).unwrap();
        }

        NetworkStack {
            network_interface: interface,
            sockets: RefCell::new(sockets),
            next_port: RefCell::new(49152),
            unused_handles: RefCell::new(unused_handles),
        }
    }

    pub fn update(&mut self, time: u32) -> bool {
        match self.network_interface.poll(
            &mut self.sockets.borrow_mut(),
            net::time::Instant::from_millis(time as i64),
        ) {
            Ok(changed) => changed == false,
            Err(e) => {
                info!("{:?}", e);
                true
            }
        }
    }

    fn get_ephemeral_port(&self) -> u16 {
        // Get the next ephemeral port
        let current_port = self.next_port.borrow().clone();

        let (next, wrap) = self.next_port.borrow().overflowing_add(1);
        *self.next_port.borrow_mut() = if wrap { 49152 } else { next };

        return current_port;
    }
}

impl<'a, 'b, 'c, 'n> embedded_nal::TcpStack for NetworkStack<'a, 'b, 'c, 'n> {
    type TcpSocket = net::socket::SocketHandle;
    type Error = NetworkError;

    fn open(&self, _mode: embedded_nal::Mode) -> Result<Self::TcpSocket, Self::Error> {
        // TODO: Handle mode?
        match self.unused_handles.borrow_mut().pop() {
            Some(handle) => {
                // Abort any active connections on the handle.
                let mut sockets = self.sockets.borrow_mut();
                let internal_socket: &mut net::socket::TcpSocket = &mut *sockets.get(handle);
                internal_socket.abort();

                Ok(handle)
            }
            None => Err(NetworkError::NoSocket),
        }
    }

    fn connect(
        &self,
        socket: Self::TcpSocket,
        remote: embedded_nal::SocketAddr,
    ) -> Result<Self::TcpSocket, Self::Error> {
        // TODO: Handle socket mode?

        let mut sockets = self.sockets.borrow_mut();
        let internal_socket: &mut net::socket::TcpSocket = &mut *sockets.get(socket);

        // If we're already in the process of connecting, ignore the request silently.
        if internal_socket.is_open() {
            return Ok(socket);
        }

        match remote.ip() {
            embedded_nal::IpAddr::V4(addr) => {
                let address = {
                    let octets = addr.octets();
                    net::wire::Ipv4Address::new(octets[0], octets[1], octets[2], octets[3])
                };
                internal_socket
                    .connect((address, remote.port()), self.get_ephemeral_port())
                    .map_err(|_| NetworkError::ConnectionFailure)?;
            }
            embedded_nal::IpAddr::V6(addr) => {
                let address = {
                    let octets = addr.segments();
                    net::wire::Ipv6Address::new(
                        octets[0], octets[1], octets[2], octets[3], octets[4], octets[5],
                        octets[6], octets[7],
                    )
                };
                internal_socket
                    .connect((address, remote.port()), self.get_ephemeral_port())
                    .map_err(|_| NetworkError::ConnectionFailure)?;
            }
        };

        Ok(socket)
    }

    fn is_connected(&self, socket: &Self::TcpSocket) -> Result<bool, Self::Error> {
        let mut sockets = self.sockets.borrow_mut();
        let socket: &mut net::socket::TcpSocket = &mut *sockets.get(*socket);

        Ok(socket.may_send() && socket.may_recv())
    }

    fn write(&self, socket: &mut Self::TcpSocket, buffer: &[u8]) -> nb::Result<usize, Self::Error> {
        // TODO: Handle the socket mode.

        let mut sockets = self.sockets.borrow_mut();
        let socket: &mut net::socket::TcpSocket = &mut *sockets.get(*socket);

        let result = socket.send_slice(buffer);

        match result {
            Ok(num_bytes) => Ok(num_bytes),
            Err(_) => Err(nb::Error::Other(NetworkError::WriteFailure)),
        }
    }

    fn read(
        &self,
        socket: &mut Self::TcpSocket,
        buffer: &mut [u8],
    ) -> nb::Result<usize, Self::Error> {
        // TODO: Handle the socket mode.

        let mut sockets = self.sockets.borrow_mut();
        let socket: &mut net::socket::TcpSocket = &mut *sockets.get(*socket);

        let result = socket.recv_slice(buffer);

        match result {
            Ok(num_bytes) => Ok(num_bytes),
            Err(_) => Err(nb::Error::Other(NetworkError::ReadFailure)),
        }
    }

    fn close(&self, socket: Self::TcpSocket) -> Result<(), Self::Error> {
        // TODO: Free the ephemeral port in use by the socket.

        let mut sockets = self.sockets.borrow_mut();
        let internal_socket: &mut net::socket::TcpSocket = &mut *sockets.get(socket);
        internal_socket.close();

        self.unused_handles.borrow_mut().push(socket).unwrap();
        Ok(())
    }
}
