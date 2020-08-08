use crate::{
    de::{
        deserialize::{self, ReceivedPacket},
        PacketReader,
    },
    minimq::{Meta, PubInfo},
    ser::serialize,
    session_state::SessionState,
};
pub use crate::properties::Property;

use embedded_nal::{Mode, SocketAddr};

pub use generic_array::typenum as consts;
use generic_array::{ArrayLength, GenericArray};

use nb;

use core::cell::RefCell;
pub use embedded_nal::{IpAddr, Ipv4Addr};

pub struct MqttClient<N, T>
where
    N: embedded_nal::TcpStack,
    T: ArrayLength<u8>,
{
    socket: RefCell<Option<N::TcpSocket>>,
    pub network_stack: N,
    state: SessionState,
    packet_reader: PacketReader<T>,
    transmit_buffer: RefCell<GenericArray<u8, T>>,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum QoS {
    AtMostOnce,
    AtLeastOnce,
    ExactlyOne,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Error<E> {
    Network(E),
    WriteFail,
    Disconnected,
    Unsupported,
    Failed(u8),
    Protocol(ProtocolError),
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ProtocolError {
    Bounds,
    DataSize,
    Invalid,
    PacketSize,
    EmptyPacket,
    Failed,
    PartialPacket,
    MalformedPacket,
    MalformedInteger,
    UnknownProperty,
    UnsupportedPacket,
}

impl<E> From<E> for Error<E> {
    fn from(e: E) -> Error<E> {
        Error::Network(e)
    }
}

impl<N, T> MqttClient<N, T>
where
    N: embedded_nal::TcpStack,
    T: ArrayLength<u8>,
{
    pub fn new<'a>(
        broker: IpAddr,
        client_id: &'a str,
        network_stack: N,
    ) -> Result<Self, Error<N::Error>> {
        // Connect to the broker's TCP port.
        let socket = network_stack.open(Mode::NonBlocking)?;

        // Next, connect to the broker over MQTT.
        let socket = network_stack.connect(socket, SocketAddr::new(broker, 1883))?;

        let mut client = MqttClient {
            network_stack: network_stack,
            socket: RefCell::new(Some(socket)),
            state: SessionState::new(broker, client_id),
            transmit_buffer: RefCell::new(GenericArray::default()),
            packet_reader: PacketReader::new(),
        };

        client.reset()?;

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

    pub fn subscribe<'a, 'b>(&mut self, topic: &'a str, _properties: &[Property<'b>]) -> Result<(), Error<N::Error>> {

        let packet_id = self.state.get_packet_identifier();
        let mut buffer = self.transmit_buffer.borrow_mut();
        let len = serialize::subscribe_message(&mut buffer, topic, packet_id).map_err(|e| Error::Protocol(e))?;

        match self.write(&buffer[..len]) {
            Ok(_) => {
                self.state.pending_subscriptions.push(packet_id).map_err(|_| Error::Unsupported)?;
                self.state.increment_packet_identifier();
                Ok(())
            },
            Err(e) => Err(e),
        }
    }

    pub fn subscriptions_pending(&self) -> bool {
        self.state.pending_subscriptions.len() == 0
    }

    pub fn publish<'a, 'b>(&self, topic: &'a str, data: &[u8], qos: QoS, properties: &[Property<'a>]) -> Result<(), Error<N::Error>> {
        // TODO: QoS support.
        assert!(qos == QoS::AtMostOnce);

        // If the socket is not connected, we can't do anything.
        if self.socket_is_connected()? == false {
            return Ok(());
        }

        let mut pub_info = PubInfo::new();
        pub_info.topic = Meta::new(topic.as_bytes());

        for property in properties {
            match property {
                Property::ResponseTopic(topic) => pub_info.response = Some(Meta::new(topic.as_bytes())),
            }
        }

        let mut buffer = self.transmit_buffer.borrow_mut();
        let len = serialize::publish_message(&mut buffer, &pub_info, data)
            .map_err(|e| Error::Protocol(e))?;
        self.write(&buffer[..len])
    }

    fn socket_is_connected(&self) -> Result<bool, N::Error> {
        let mut socket_ref = self.socket.borrow_mut();
        let socket = socket_ref.take().unwrap();

        let connected = self.network_stack.is_connected(&socket)?;

        socket_ref.replace(socket);

        Ok(connected)
    }

    fn reset(&mut self) -> Result<(), Error<N::Error>> {
        // TODO: Handle connection failures?
        self.connect_socket(true)?;
        self.state.reset();
        self.packet_reader.reset();

        let mut buffer = self.transmit_buffer.borrow_mut();
        let len = serialize::connect_message(
            &mut buffer,
            self.state.client_id.as_str().as_bytes(),
            self.state.keep_alive_interval,
        )
        .map_err(|e| Error::Protocol(e))?;
        self.write(&buffer[..len])?;

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
            .connect(socket, SocketAddr::new(self.state.broker, 1883))?;

        // Store the new socket for future use.
        socket_ref.replace(socket);

        Ok(())
    }

    fn handle_packet<F>(&mut self, packet: ReceivedPacket, f: &F) -> Result<(), Error<N::Error>>
    where
        F: Fn(&Self, &PubInfo, &[u8]),
    {
        if !self.state.connected {
            if let ReceivedPacket::ConnAck(acknowledge) = packet {
                if acknowledge.reason_code != 0 {
                    return Err(Error::Failed(acknowledge.reason_code));
                }

                if acknowledge.session_present {
                    // We do not currently support saved session state.
                    return Err(Error::Unsupported);
                } else {
                    self.state.reset();
                }

                self.state.connected = true;

                // TODO: Handle properties in the ConnAck.

                return Ok(());
            } else {
                // It is a protocol error to receive anything else when not connected.
                // TODO: Verify it is a protocol error.
                return Err(Error::Protocol(ProtocolError::Invalid));
            }
        }

        match packet {
            ReceivedPacket::Publish(info) => {
                // Call a handler function to deal with the received data.
                let payload = self
                    .packet_reader
                    .payload()
                    .map_err(|e| Error::Protocol(e))?;
                f(self, &info, payload);

                Ok(())
            }

            ReceivedPacket::SubAck(subscribe_acknowledge) => {
                match self.state.pending_subscriptions.iter().position(
                        |v| *v == subscribe_acknowledge.packet_identifier)
                {
                    None => return Err(Error::Protocol(ProtocolError::Invalid)),
                    Some(index) => self.state.pending_subscriptions.swap_remove(index),
                };

                if subscribe_acknowledge.reason_code != 0 {
                    return Err(Error::Failed(subscribe_acknowledge.reason_code));
                }

                Ok(())
            }

            _ => Err(Error::Unsupported),
        }
    }

    pub fn poll<F>(&mut self, f: F) -> Result<(), Error<N::Error>>
    where
        F: Fn(&Self, &PubInfo, &[u8]),
    {
        // If the socket is not connected, we can't do anything.
        if self.socket_is_connected()? == false {
            // TODO: Handle a session timeout.
            self.reset()?;

            return Ok(());
        }

        let mut buf: [u8; 1024] = [0; 1024];
        let received = self.read(&mut buf)?;
        let mut processed = 0;
        while processed < received {
            match self.packet_reader.slurp(&buf[processed..received]) {
                Ok(count) => processed += count,

                Err(_) => {
                    // TODO: We should handle recoverable errors better.
                    self.reset()?;
                    return Err(Error::Disconnected);
                }
            }

            // Handle any received packets.
            if self.packet_reader.packet_available() {
                // TODO: Handle deserialize errors properly.
                let packet = deserialize::parse_message(&mut self.packet_reader)
                    .map_err(|e| Error::Protocol(e))?;
                self.packet_reader
                    .pop_packet()
                    .map_err(|e| Error::Protocol(e))?;
                self.handle_packet(packet, &f)?;
            }
        }

        Ok(())
    }
}
