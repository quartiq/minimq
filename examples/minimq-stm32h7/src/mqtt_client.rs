use embedded_nal::{
    Mode,
    SocketAddr,
};

use nb;

use core::cell::RefCell;
pub use embedded_nal::{IpAddr, Ipv4Addr};
use heapless::{String, consts};

pub struct MqttClient<N>
where
    N: embedded_nal::TcpStack,
{
    socket: RefCell<N::TcpSocket>,
    pub network_stack: N,
    protocol: RefCell<minimq::Protocol>,
    id: String<consts::U32>
}

#[derive(Debug)]
pub enum Error<E> {
    Network(E),
    WriteFail,
}

impl<E> From<E> for Error<E>
{
    fn from(e: E) -> Error<E> {
        Error::Network(e)
    }
}

impl<N> MqttClient<N>
where
    N: embedded_nal::TcpStack,
{
    pub fn new<'a>(broker: IpAddr, client_id: &'a str, network_stack: N) -> Result<Self,
    Error<N::Error>> {
        // Connect to the broker's TCP port.
        let socket = network_stack.open(Mode::NonBlocking)?;

        // Next, connect to the broker over MQTT.
        let protocol = minimq::Protocol::new();
        let socket = network_stack.connect(socket, SocketAddr::new(broker, 1883))?;

        Ok(MqttClient {
            network_stack: network_stack,
            socket: RefCell::new(socket),
            protocol: RefCell::new(protocol),
            id: String::from(client_id),
        })
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

    pub fn publish<'a>(&self, topic: &'a str, data: &[u8]) -> Result<(), Error<N::Error>> {
        // If the socket is not connected, we can't do anything.
        if self.network_stack.is_connected(&self.socket.borrow())? == false {
            return Ok(());
        }

        let mut pub_info = minimq::PubInfo::new();
        pub_info.topic = minimq::Meta::new(topic.as_bytes());

        let mut protocol = self.protocol.borrow_mut();
        self.write(protocol.publish(&pub_info, data))
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
            let connect_message = protocol.connect(self.id.as_bytes(), 10);
            self.write(connect_message)?;
        }

        // TODO: Handle socket disconnections?

        let mut buf = [0; 1024];
        let received = self.read(&mut buf)?;
        let mut processed = 0;
        while processed < received {
            // TODO: Handle this error.
            let (read, reply) = protocol.receive(&buf[processed..]).unwrap();
            processed += read;

            if let Some(reply) = reply {
                self.write(reply)?;
            }

            if let Some((publish_info, payload)) = protocol.handle() {
                f(publish_info.topic.get(), payload);
            }
        }

        Ok(())
    }

}
