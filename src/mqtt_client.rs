use crate::minimq;

use embedded_nal::{Mode, SocketAddr};

use nb;

use core::cell::RefCell;
pub use embedded_nal::{IpAddr, Ipv4Addr};
use heapless::{consts, String, Vec};

pub struct MqttClient<'a, N>
where
    N: embedded_nal::TcpStack,
{
    socket: RefCell<Option<N::TcpSocket>>,
    pub network_stack: N,
    protocol: RefCell<minimq::Protocol<'a>>,
    id: String<consts::U32>,
    transmit_buffer: Vec<u8, consts::U512>,
    broker: IpAddr,
}

#[derive(Debug)]
pub enum Error<E> {
    Network(E),
    WriteFail,
    Protocol(minimq::Error),
    Disconnected,
}

impl<E> From<E> for Error<E> {
    fn from(e: E) -> Error<E> {
        Error::Network(e)
    }
}

impl<'a, N> MqttClient<'a, N>
where
    N: embedded_nal::TcpStack,
{
    pub fn new<'b>(
        broker: IpAddr,
        client_id: &'b str,
        network_stack: N,
        receive_buffer: &'a mut [u8],
    ) -> Result<Self, Error<N::Error>> {
        // Connect to the broker's TCP port.
        let socket = network_stack.open(Mode::NonBlocking)?;

        // Next, connect to the broker over MQTT.
        let protocol = minimq::Protocol::new(receive_buffer);
        let socket = network_stack.connect(socket, SocketAddr::new(broker, 1883))?;

        let mut client = MqttClient {
            network_stack: network_stack,
            socket: RefCell::new(Some(socket)),
            protocol: RefCell::new(protocol),
            id: String::from(client_id),
            transmit_buffer: Vec::new(),
            broker: broker,
        };

        client.transmit_buffer.resize_default(512).unwrap();

        Ok(client)
    }

    fn read(&self, mut buf: &mut [u8]) -> Result<usize, Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let mut socket = socket_ref.take().unwrap();
        let read = nb::block!(self.network_stack.read(&mut socket, &mut buf)).unwrap();

        // Put the socket back into the option.
        socket_ref.replace(socket);

        Ok(read)
    }

    fn write(&self, buf: &[u8]) -> Result<(), Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let mut socket = socket_ref.take().unwrap();
        let written = nb::block!(self.network_stack.write(&mut socket, &buf)).unwrap();

        // Put the socket back into the option.
        socket_ref.replace(socket);

        if written != buf.len() {
            Err(Error::WriteFail)
        } else {
            Ok(())
        }
    }

    // TODO: Add subscribe support.

    pub fn publish<'b>(&mut self, topic: &'b str, data: &[u8]) -> Result<(), Error<N::Error>> {
        // If the socket is not connected, we can't do anything.
        if self.socket_is_connected()? == false {
            return Ok(());
        }

        let mut pub_info = minimq::PubInfo::new();
        pub_info.topic = minimq::Meta::new(topic.as_bytes());

        let protocol = self.protocol.borrow_mut();
        let len = protocol
            .publish(&mut self.transmit_buffer, &pub_info, data)
            .map_err(|e| Error::Protocol(e))?;
        self.write(&self.transmit_buffer[..len])
    }

    fn socket_is_connected(&self) -> Result<bool, N::Error> {
        let mut socket_ref = self.socket.borrow_mut();
        let socket = socket_ref.take().unwrap();

        let connected = self.network_stack.is_connected(&socket)?;

        socket_ref.replace(socket);

        Ok(connected)
    }

    fn reset(&mut self) -> Result<(), Error<N::Error>> {
        self.connect_socket(true)?;
        self.protocol.borrow_mut().reset();

        Ok(())
    }

    fn connect_socket(&mut self, new_socket: bool) -> Result<(), Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let socket = socket_ref.take().unwrap();

        // Close the socket. We need to reset the socket state.
        let socket = if new_socket {
            self.network_stack.close(socket)?;
            self.network_stack.open(Mode::NonBlocking)?
        } else {
            socket
        };

        // Connect to the broker's TCP port with a new socket.
        // TODO: Limit the time between connect attempts to prevent network spam.
        let socket = self
            .network_stack
            .connect(socket, SocketAddr::new(self.broker, 1883))?;

        // Store the new socket for future use.
        socket_ref.replace(socket);

        Ok(())
    }

    pub fn poll<F>(&mut self, f: F) -> Result<(), Error<N::Error>>
    where
        F: Fn(&[u8], &[u8]),
    {
        // If the socket is not connected, we can't do anything.
        if self.socket_is_connected()? == false {
            self.connect_socket(false)?;

            // If we're in the initialization state, there's nothing to do. We're waiting for an
            // initial connection. Otherwise, we had an active connection, but it's gone now. We
            // need to re-open the socket and re-establish a connection with the broker.
            if self.protocol.borrow().state() != minimq::ProtocolState::Init {
                self.reset()?;
            }

            return Ok(());
        }

        // If we're in the initialization state, we need to send a connect packet to connect with
        // the broker.
        if self.protocol.borrow().state() == minimq::ProtocolState::Init {
            // Connect to the MQTT broker.
            let mut protocol = self.protocol.borrow_mut();
            let len = protocol
                .connect(&mut self.transmit_buffer, self.id.as_bytes(), 10)
                .map_err(|e| Error::Protocol(e))?;
            self.write(&self.transmit_buffer[..len])?;
        }

        let mut buf: [u8; 1024] = [0; 1024];
        let received = self.read(&mut buf)?;
        let mut processed = 0;
        while processed < received {
            // TODO: Handle this error.
            let result = self.protocol.borrow_mut().receive(&buf[processed..]);

            let read = match result {
                Ok(count) => count,
                Err(_) => {
                    // If we got an error during parsing, reset the connection to the broker.
                    self.reset()?;
                    return Err(Error::Disconnected);
                }
            };

            processed += read;

            // TODO: Expose the client to the user handler to allow them to easily publish within
            // the closure.
            let result = self
                .protocol
                .borrow_mut()
                .handle(|_protocol, publish_info, payload| {
                    f(publish_info.topic.get(), payload);
                });

            if let Err(_) = result {
                // If we got an error during packet processing, reset the connection with the broker.
                self.reset()?;
                return Err(Error::Disconnected);
            }
        }

        Ok(())
    }
}
