//! Network Interface Holder
//!
//! # Design
//! The embedded-nal network interface is abstracted away into a separate struct to facilitate
//! simple ownership semantics of reading and writing to the network stack. This allows the network
//! stack to be used to transmit buffers that may be stored internally in other structs without
//! violating Rust's borrow rules.
use embedded_nal::{nb, SocketAddr, TcpClientStack};
use embedded_time::Clock;

use crate::session_state::SessionState;

use crate::Error;

/// Simple structure for maintaining state of the network connection.
pub(crate) struct InterfaceHolder<N: TcpClientStack> {
    socket: Option<N::TcpSocket>,
    network_stack: N,
}

impl<N> InterfaceHolder<N>
where
    N: TcpClientStack,
{
    /// Construct a new network holder utility.
    pub fn new(stack: N) -> Self {
        Self {
            socket: None,
            network_stack: stack,
        }
    }

    /// Determine if an TCP connection exists and is connected.
    pub fn tcp_connected(&mut self) -> Result<bool, Error<N::Error>> {
        if self.socket.is_none() {
            return Ok(false);
        }

        let socket = self.socket.as_ref().unwrap();
        self.network_stack
            .is_connected(socket)
            .map_err(|err| Error::Network(err))
    }

    /// Allocate a new TCP socket.
    ///
    /// # Note
    /// If a TCP socket was previously open, it will be closed and a new socket will be allocated.
    pub fn allocate_socket(&mut self) -> Result<(), Error<N::Error>> {
        if let Some(socket) = self.socket.take() {
            self.network_stack
                .close(socket)
                .map_err(|err| Error::Network(err))?;
        }

        // Allocate a new socket to use and begin connecting it.
        self.socket.replace(
            self.network_stack
                .socket()
                .map_err(|err| Error::Network(err))?,
        );

        Ok(())
    }

    /// Connect the TCP socket to a remote address.
    ///
    /// # Args
    /// * `remote` - The address of the remote to connect to.
    pub fn connect(&mut self, remote: SocketAddr) -> Result<(), Error<N::Error>> {
        let socket = self.socket.as_mut().ok_or(Error::NotReady)?;
        self.network_stack
            .connect(socket, remote)
            .map_err(|err| match err {
                nb::Error::WouldBlock => Error::WriteFail,
                nb::Error::Other(err) => Error::Network(err),
            })
    }

    /// Write an MQTT control packet to the interface.
    ///
    /// # Args
    /// * `session_state` - The MQTT session state to update when the control packet is written.
    /// * `clock` - The clock used for keeping time in the system.
    /// * `packet` - The packet to write.
    pub fn write_packet<C: Clock>(
        &mut self,
        session_state: &mut SessionState<C>,
        clock: &C,
        packet: &[u8],
    ) -> Result<(), Error<N::Error>> {
        let socket = self.socket.as_mut().ok_or(Error::NotReady)?;
        self.network_stack
            .send(socket, &packet)
            .map_err(|err| match err {
                nb::Error::WouldBlock => Error::WriteFail,
                nb::Error::Other(err) => Error::Network(err),
            })
            .and_then(|written| {
                if written != packet.len() {
                    Err(Error::WriteFail)
                } else {
                    session_state.register_transmission(clock.try_now()?);
                    Ok(())
                }
            })
    }

    /// Read data from the TCP interface.
    ///
    /// # Args
    /// * `buf` - A location to store read data into.
    ///
    /// # Returns
    /// The number of bytes successfully read.
    pub fn read(&mut self, mut buf: &mut [u8]) -> Result<usize, Error<N::Error>> {
        // Atomically access the socket.
        let socket = self.socket.as_mut().ok_or(Error::NotReady)?;
        let result = self.network_stack.receive(socket, &mut buf);

        result.or_else(|err| match err {
            nb::Error::WouldBlock => Ok(0),
            nb::Error::Other(err) => Err(Error::Network(err)),
        })
    }
}
