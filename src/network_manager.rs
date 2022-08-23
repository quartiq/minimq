//! Network Interface Holder
//!
//! # Design
//! The embedded-nal network interface is abstracted away into a separate struct to facilitate
//! simple ownership semantics of reading and writing to the network stack. This allows the network
//! stack to be used to transmit buffers that may be stored internally in other structs without
//! violating Rust's borrow rules.
use embedded_nal::{nb, SocketAddr, TcpClientStack};
use heapless::Vec;

use crate::Error;

/// Simple structure for maintaining state of the network connection.
pub(crate) struct InterfaceHolder<TcpStack: TcpClientStack, const MSG_SIZE: usize> {
    socket: Option<TcpStack::TcpSocket>,
    network_stack: TcpStack,
    pending_write: Option<Vec<u8, MSG_SIZE>>,
}

impl<TcpStack, const MSG_SIZE: usize> InterfaceHolder<TcpStack, MSG_SIZE>
where
    TcpStack: TcpClientStack,
{
    /// Construct a new network holder utility.
    pub fn new(stack: TcpStack) -> Self {
        Self {
            socket: None,
            network_stack: stack,
            pending_write: None,
        }
    }

    /// Determine if there is a pending packet write that needs to be completed.
    pub fn has_pending_write(&self) -> bool {
        self.pending_write.is_some()
    }

    /// Determine if an TCP connection exists and is connected.
    pub fn tcp_connected(&mut self) -> Result<bool, Error<TcpStack::Error>> {
        if self.socket.is_none() {
            return Ok(false);
        }

        let socket = self.socket.as_ref().unwrap();
        self.network_stack
            .is_connected(socket)
            .map_err(Error::Network)
    }

    /// Allocate a new TCP socket.
    ///
    /// # Note
    /// If a TCP socket was previously open, it will be closed and a new socket will be allocated.
    pub fn allocate_socket(&mut self) -> Result<(), Error<TcpStack::Error>> {
        if let Some(socket) = self.socket.take() {
            self.network_stack.close(socket).map_err(Error::Network)?;
        }

        // Allocate a new socket to use and begin connecting it.
        self.socket
            .replace(self.network_stack.socket().map_err(Error::Network)?);

        Ok(())
    }

    /// Connect the TCP socket to a remote address.
    ///
    /// # Args
    /// * `remote` - The address of the remote to connect to.
    pub fn connect(&mut self, remote: SocketAddr) -> Result<(), Error<TcpStack::Error>> {
        let socket = self.socket.as_mut().ok_or(Error::NotReady)?;

        // Drop any pending unfinished packets, as we're establishing a new connection.
        self.pending_write.take();

        self.network_stack
            .connect(socket, remote)
            .map_err(|err| match err {
                nb::Error::WouldBlock => Error::WriteFail,
                nb::Error::Other(err) => Error::Network(err),
            })
    }

    /// Write data to the interface.
    ///
    /// # Args
    /// * `packet` - The data to write.
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error<TcpStack::Error>> {
        // If there's an unfinished write pending, it's invalid to try to write new data. The
        // previous write must first be completed.
        assert!(self.pending_write.is_none());

        let socket = self.socket.as_mut().ok_or(Error::NotReady)?;
        self.network_stack
            .send(socket, data)
            .or_else(|err| match err {
                nb::Error::WouldBlock => Ok(0),
                nb::Error::Other(err) => Err(Error::Network(err)),
            })
            .map(|written| {
                crate::trace!("Wrote: {:0x?}", &data[..written]);
                if written != data.len() {
                    // Note(unwrap): The packet should always be smaller than a single message.
                    self.pending_write
                        .replace(Vec::from_slice(&data[written..]).unwrap());
                }
            })
    }

    /// Finish writing an MQTT control packet to the interface if one exists.
    pub fn finish_write(&mut self) -> Result<(), Error<TcpStack::Error>> {
        if let Some(ref packet) = self.pending_write.take() {
            self.write(packet.as_slice())?;
        }

        Ok(())
    }

    /// Read data from the TCP interface.
    ///
    /// # Args
    /// * `buf` - A location to store read data into.
    ///
    /// # Returns
    /// The number of bytes successfully read.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error<TcpStack::Error>> {
        // Atomically access the socket.
        let socket = self.socket.as_mut().ok_or(Error::NotReady)?;
        let result = self.network_stack.receive(socket, buf);

        #[cfg(features = "logging")]
        if let Ok(len) = result {
            let data = &buf[..len];
            crate::trace!("Read: {:0x?}", data);
        }

        result.or_else(|err| match err {
            nb::Error::WouldBlock => Ok(0),
            nb::Error::Other(err) => Err(Error::Network(err)),
        })
    }
}
