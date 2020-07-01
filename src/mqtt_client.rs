use crate::minimq;

use embedded_nal::{
    Mode,
    SocketAddr,
};

use nb;

use core::cell::RefCell;
pub use embedded_nal::{IpAddr, Ipv4Addr};
use heapless::{Vec, String, consts};

pub struct MqttClient<'a, N>
where
    N: embedded_nal::TcpStack,
{
    socket: RefCell<N::TcpSocket>,
    pub network_stack: N,
    protocol: RefCell<minimq::Protocol<'a>>,
    id: String<consts::U32>,
    transmit_buffer: Vec<u8, consts::U512>,
}

#[derive(Debug)]
pub enum Error<E> {
    Network(E),
    WriteFail,
    Protocol(minimq::Error),

}

impl<E> From<E> for Error<E>
{
    fn from(e: E) -> Error<E> {
        Error::Network(e)
    }
}

impl<'a, N> MqttClient<'a, N>
where
    N: embedded_nal::TcpStack,
{
    pub fn new<'b>(broker: IpAddr, client_id: &'b str, network_stack: N, receive_buffer: &'a mut [u8]) -> Result<Self,
    Error<N::Error>> {
        // Connect to the broker's TCP port.
        let socket = network_stack.open(Mode::NonBlocking)?;

        // Next, connect to the broker over MQTT.
        let protocol = minimq::Protocol::new(receive_buffer);
        let socket = network_stack.connect(socket, SocketAddr::new(broker, 1883))?;

        let mut client = MqttClient {
            network_stack: network_stack,
            socket: RefCell::new(socket),
            protocol: RefCell::new(protocol),
            id: String::from(client_id),
            transmit_buffer: Vec::new(),
        };

        client.transmit_buffer.resize_default(512).unwrap();

        Ok(client)
    }

    fn read(&self, mut buf: &mut [u8]) -> Result<usize, Error<N::Error>> {
        let read = nb::block!(self.network_stack.read(&mut self.socket.borrow_mut(), &mut buf)).unwrap();

        Ok(read)
    }

    fn write(&self, buf: & [u8]) -> Result<(), Error<N::Error>> {
        let written = nb::block!(self.network_stack.write(&mut self.socket.borrow_mut(), &buf)).unwrap();

        if written != buf.len() {
            Err(Error::WriteFail)
        } else {
            Ok(())
        }
    }

    // TODO: Add subscribe support.

    pub fn publish<'b>(&mut self, topic: &'b str, data: &[u8]) -> Result<(), Error<N::Error>> {
        // If the socket is not connected, we can't do anything.
        if self.network_stack.is_connected(&self.socket.borrow())? == false {
            return Ok(());
        }

        let mut pub_info = minimq::PubInfo::new();
        pub_info.topic = minimq::Meta::new(topic.as_bytes());

        let protocol = self.protocol.borrow_mut();
        let len = protocol.publish(&mut self.transmit_buffer, &pub_info, data).map_err(|e|
                Error::Protocol(e))?;
        self.write(&self.transmit_buffer[..len])
    }

    pub fn poll<F>(&mut self, f: F) -> Result<(), Error<N::Error>>
    where
        F: Fn(&[u8], &[u8]),
    {
        // If the socket is not connected, we can't do anything.
        if self.network_stack.is_connected(&self.socket.borrow())? == false {
            return Ok(());
        }

        // Connect to the MQTT broker.
        let mut protocol = self.protocol.borrow_mut();
        if protocol.state() == minimq::ProtocolState::Close {
            let len = protocol.connect(&mut self.transmit_buffer, self.id.as_bytes(),
                    10).map_err(|e| Error::Protocol(e))?;
            self.write(&self.transmit_buffer[..len])?;
        }

        // TODO: Handle socket disconnections?

        let mut buf: [u8; 1024] = [0; 1024];
        let received = self.read(&mut buf)?;
        let mut processed = 0;
        while processed < received {
            // TODO: Handle this error.
            let read = protocol.receive(&buf[processed..]).map_err(|e| Error::Protocol(e))?;
            processed += read;

            // TODO: Expose the client to the user handler to allow them to easily publish within
            // the closure.
            protocol.handle(|_protocol, publish_info, payload| {
                f(publish_info.topic.get(), payload);
            }).map_err(|e| Error::Protocol(e))?;
        }

        Ok(())
    }
}
