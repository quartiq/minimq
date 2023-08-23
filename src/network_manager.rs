//! Network Interface Holder
//!
//! # Design
//! The embedded-nal network interface is abstracted away into a separate struct to facilitate
//! simple ownership semantics of reading and writing to the network stack. This allows the network
//! stack to be used to transmit buffers that may be stored internally in other structs without
//! violating Rust's borrow rules.
use crate::{message_types::ControlPacket, packets::Pub};
use embedded_nal::{nb, SocketAddr, TcpClientStack, TcpError};
use serde::Serialize;

use crate::{Error, ProtocolError};

/// Simple structure for maintaining state of the network connection.
pub(crate) struct InterfaceHolder<'a, TcpStack: TcpClientStack> {
    socket: Option<TcpStack::TcpSocket>,
    network_stack: TcpStack,
    tx_buffer: &'a mut [u8],
    pending_write: Option<(usize, usize)>,
    connection_died: bool,
}

impl<'a, TcpStack> InterfaceHolder<'a, TcpStack>
where
    TcpStack: TcpClientStack,
{
    /// Construct a new network holder utility.
    pub fn new(stack: TcpStack, tx_buffer: &'a mut [u8]) -> Self {
        Self {
            socket: None,
            network_stack: stack,
            pending_write: None,
            connection_died: false,
            tx_buffer,
        }
    }

    pub fn socket_was_closed(&mut self) -> bool {
        let was_closed = self.connection_died;
        if was_closed {
            self.pending_write.take();
        }
        self.connection_died = false;
        was_closed
    }

    /// Determine if there is a pending packet write that needs to be completed.
    pub fn has_pending_write(&self) -> bool {
        self.pending_write.is_some()
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
    pub fn connect(&mut self, remote: SocketAddr) -> Result<bool, Error<TcpStack::Error>> {
        if self.socket.is_none() {
            self.allocate_socket()?;
        }

        let socket = self.socket.as_mut().unwrap();

        // Drop any pending unfinished packets, as we're establishing a new connection.
        self.pending_write.take();

        match self.network_stack.connect(socket, remote) {
            Ok(_) => Ok(true),
            Err(nb::Error::WouldBlock) => Ok(false),
            Err(nb::Error::Other(err)) => Err(Error::Network(err)),
        }
    }

    /// Write data to the interface.
    ///
    /// # Args
    /// * `packet` - The data to write.
    fn commit_write(&mut self, start: usize, len: usize) -> Result<(), Error<TcpStack::Error>> {
        // If there's an unfinished write pending, it's invalid to try to write new data. The
        // previous write must first be completed.
        assert!(self.pending_write.is_none());

        let socket = self.socket.as_mut().ok_or(Error::NotReady)?;
        self.network_stack
            .send(socket, &self.tx_buffer[start..][..len])
            .or_else(|err| match err {
                nb::Error::WouldBlock => Ok(0),
                nb::Error::Other(err) if err.kind() == embedded_nal::TcpErrorKind::PipeClosed => {
                    self.connection_died = true;
                    Ok(0)
                }
                nb::Error::Other(err) => Err(Error::Network(err)),
            })
            .map(|written| {
                crate::trace!("Wrote: {:0x?}", &self.tx_buffer[..written]);
                if written != len {
                    crate::warn!("Saving pending data. Wrote {written} of {len}");
                    self.pending_write.replace((written, len));
                }
            })
    }

    pub fn write_multipart(
        &mut self,
        head: &[u8],
        tail: &[u8],
    ) -> Result<(), Error<TcpStack::Error>> {
        self.tx_buffer[..head.len()].copy_from_slice(head);
        self.tx_buffer[head.len()..][..tail.len()].copy_from_slice(tail);
        self.commit_write(0, head.len() + tail.len())
    }

    /// Send an MQTT control packet over the interface.
    ///
    /// # Args
    /// * `packet` - The packet to transmit.
    pub fn send_packet<T: Serialize + ControlPacket + core::fmt::Debug>(
        &mut self,
        packet: &T,
    ) -> Result<&[u8], Error<TcpStack::Error>> {
        // If there's an unfinished write pending, it's invalid to try to write new data. The
        // previous write must first be completed.
        assert!(self.pending_write.is_none());

        crate::info!("Sending: {:?}", packet);
        let (offset, packet) = crate::ser::MqttSerializer::to_buffer_meta(self.tx_buffer, packet)
            .map_err(ProtocolError::from)?;
        let len = packet.len();

        self.commit_write(offset, len)?;
        Ok(&self.tx_buffer[offset..][..len])
    }

    pub fn send_pub<P: crate::publication::ToPayload>(
        &mut self,
        pub_packet: &Pub<'_, P>,
    ) -> Result<&[u8], Error<TcpStack::Error>> {
        // If there's an unfinished write pending, it's invalid to try to write new data. The
        // previous write must first be completed.
        assert!(self.pending_write.is_none());

        crate::info!("Sending: {:?}", pub_packet);
        let (offset, packet) =
            crate::ser::MqttSerializer::pub_to_buffer_meta(self.tx_buffer, pub_packet)
                .map_err(ProtocolError::from)?;

        let len = packet.len();

        self.commit_write(offset, len)?;
        Ok(&self.tx_buffer[offset..][..len])
    }

    /// Finish writing an MQTT control packet to the interface if one exists.
    pub fn finish_write(&mut self) -> Result<(), Error<TcpStack::Error>> {
        if let Some((head, tail)) = self.pending_write.take() {
            self.commit_write(head, tail - head)?;
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

        #[cfg(feature = "logging")]
        if let Ok(len) = result {
            if len > 0 {
                let data = &buf[..len];
                crate::trace!("Read: {:0x?}", data);
            }
        }

        result.or_else(|err| match err {
            nb::Error::WouldBlock => Ok(0),
            nb::Error::Other(err) if err.kind() == embedded_nal::TcpErrorKind::PipeClosed => {
                self.connection_died = true;
                Ok(0)
            }
            nb::Error::Other(err) => Err(Error::Network(err)),
        })
    }
}
