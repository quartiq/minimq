pub use crate::properties::Property;
use crate::{
    de::{deserialize::ReceivedPacket, PacketReader},
    ser::serialize,
    session_state::SessionState,
};
use log::{debug, error, info};

use embedded_nal::{Mode, SocketAddr};

pub use generic_array::typenum as consts;
use generic_array::{ArrayLength, GenericArray};

use nb;

use core::cell::RefCell;
pub use embedded_nal::{IpAddr, Ipv4Addr};

use heapless::String;

pub struct MqttClient<N, T>
where
    N: embedded_nal::TcpStack,
    T: ArrayLength<u8>,
{
    socket: RefCell<Option<N::TcpSocket>>,
    pub network_stack: N,
    state: RefCell<SessionState>,
    packet_reader: PacketReader<T>,
    transmit_buffer: RefCell<GenericArray<u8, T>>,
    connect_sent: bool,
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
    NotReady,
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
    Failed,
    PacketSize,
    MalformedPacket,
    MalformedInteger,
    UnknownProperty,
    UnsupportedPacket,
    BufferSize,
    InvalidProperty,
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
            state: RefCell::new(SessionState::new(broker, client_id)),
            transmit_buffer: RefCell::new(GenericArray::default()),
            packet_reader: PacketReader::new(),
            connect_sent: false,
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

    pub fn subscribe<'a, 'b>(
        &self,
        topic: &'a str,
        _properties: &[Property<'b>],
    ) -> Result<(), Error<N::Error>> {
        let mut state = self.state.borrow_mut();
        let packet_id = state.get_packet_identifier();

        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::subscribe_message(&mut buffer, topic, packet_id, &[])
            .map_err(|e| Error::Protocol(e))?;

        match self.write(packet) {
            Ok(_) => {
                info!("Subscribing to `{}`: {}", topic, packet_id);
                state
                    .pending_subscriptions
                    .push(packet_id)
                    .map_err(|_| Error::Unsupported)?;
                state.increment_packet_identifier();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn subscriptions_pending(&self) -> bool {
        self.state.borrow().pending_subscriptions.len() != 0
    }

    pub fn publish<'a, 'b>(
        &self,
        topic: &'a str,
        data: &[u8],
        qos: QoS,
        properties: &[Property<'a>],
    ) -> Result<(), Error<N::Error>> {
        // TODO: QoS support.
        assert!(qos == QoS::AtMostOnce);

        // If the socket is not connected, we can't do anything.
        if self.socket_can_communicate()? == false {
            return Ok(());
        }

        info!(
            "Publishing to `{}`: {:?} Props: {:?}",
            topic, data, properties
        );

        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::publish_message(&mut buffer, topic, data, properties)
            .map_err(|e| Error::Protocol(e))?;
        self.write(packet)
    }

    fn socket_can_communicate(&self) -> Result<bool, N::Error> {
        let mut socket_ref = self.socket.borrow_mut();
        let socket = socket_ref.take().unwrap();

        let connected = self.network_stack.is_connected(&socket)?;

        socket_ref.replace(socket);

        Ok(connected)
    }

    fn reset(&mut self) -> Result<(), Error<N::Error>> {
        self.state.borrow_mut().reset();
        self.packet_reader.reset();
        self.connect_sent = false;

        Ok(())
    }

    fn connect_to_broker(&mut self) -> Result<(), Error<N::Error>> {
        self.reset()?;

        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::connect_message(
            &mut buffer,
            self.state.borrow().client_id.as_str().as_bytes(),
            self.state.borrow().keep_alive_interval,
            &[Property::MaximumPacketSize(
                self.packet_reader.maximum_packet_length() as u32,
            )],
        )
        .map_err(|e| Error::Protocol(e))?;
        self.write(packet)?;

        self.connect_sent = true;

        Ok(())
    }

    fn handle_packet<'p, F>(&self, packet: ReceivedPacket<'p>, f: &F) -> Result<(), Error<N::Error>>
    where
        for<'a> F: Fn(&Self, &'a str, &[u8], &[Property<'a>]),
    {
        let mut state = self.state.borrow_mut();
        if !state.connected {
            if let ReceivedPacket::ConnAck(acknowledge) = packet {
                if acknowledge.reason_code != 0 {
                    return Err(Error::Failed(acknowledge.reason_code));
                }

                if acknowledge.session_present {
                    // We do not currently support saved session state.
                    return Err(Error::Unsupported);
                }

                state.connected = true;

                // TODO: Handle properties in the ConnAck.
                for property in acknowledge.properties {
                    match property {
                        Property::MaximumPacketSize(size) => {
                            state.maximum_packet_size.replace(size);
                        }
                        Property::AssignedClientIdentifier(id) => {
                            state.client_id = String::from(id);
                        }
                        Property::ServerKeepAlive(keep_alive) => {
                            state.keep_alive_interval = keep_alive;
                        }
                        prop => info!("Ignoring property: {:?}", prop),
                    };
                }

                return Ok(());
            } else {
                // It is a protocol error to receive anything else when not connected.
                // TODO: Verify it is a protocol error.
                error!(
                    "Received invalid packet outside of connected state: {:?}",
                    packet
                );
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
                f(self, info.topic, payload, &info.properties);

                Ok(())
            }

            ReceivedPacket::SubAck(subscribe_acknowledge) => {
                match state
                    .pending_subscriptions
                    .iter()
                    .position(|v| *v == subscribe_acknowledge.packet_identifier)
                {
                    None => {
                        error!("Got bad suback: {:?}", subscribe_acknowledge);
                        return Err(Error::Protocol(ProtocolError::Invalid));
                    }
                    Some(index) => state.pending_subscriptions.swap_remove(index),
                };

                if subscribe_acknowledge.reason_code != 0 {
                    return Err(Error::Failed(subscribe_acknowledge.reason_code));
                }

                Ok(())
            }

            _ => Err(Error::Unsupported),
        }
    }

    fn connect_socket(&mut self) -> Result<(), Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let socket = socket_ref.take().unwrap();

        // Connect to the broker's TCP port with a new socket.
        // TODO: Limit the time between connect attempts to prevent network spam.
        let socket = self
            .network_stack
            .connect(socket, SocketAddr::new(self.state.borrow().broker, 1883))?;

        // Store the new socket for future use.
        socket_ref.replace(socket);

        Ok(())
    }

    pub fn poll<F>(&mut self, f: F) -> Result<(), Error<N::Error>>
    where
        for<'a> F: Fn(&Self, &'a str, &[u8], &[Property<'a>]),
    {
        debug!("Polling MQTT interface");

        // If the socket is not connected, we can't do anything.
        if self.socket_can_communicate()? == false {
            self.connect_socket()?;
            return Ok(());
        }

        // If we are not yet connected to the MQTT broker, we can't do anything.
        if !self.state.borrow().connected {
            if !self.connect_sent {
                self.connect_to_broker()?;
            }

            return Ok(());
        }

        let mut buf: [u8; 1024] = [0; 1024];
        let received = self.read(&mut buf)?;
        debug!("Received {} bytes", received);

        let mut processed = 0;
        while processed < received {
            match self.packet_reader.slurp(&buf[processed..received]) {
                Ok(count) => {
                    debug!("Processed {} bytes", count);
                    processed += count
                }

                Err(_) => {
                    // TODO: We should handle recoverable errors better.
                    self.reset()?;
                    return Err(Error::Disconnected);
                }
            }

            // Handle any received packets.
            while self.packet_reader.packet_available() {
                // TODO: Handle deserialize errors properly.
                let packet = ReceivedPacket::parse_message(&self.packet_reader)
                    .map_err(|e| Error::Protocol(e))?;

                info!("Received {:?}", packet);
                self.handle_packet(packet, &f)?;
                self.packet_reader
                    .pop_packet()
                    .map_err(|e| Error::Protocol(e))?;
            }
        }

        Ok(())
    }
}
