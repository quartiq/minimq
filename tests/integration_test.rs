use minimq::{consts, MqttClient, Property, QoS};
use std::io::{Write, Read};

use embedded_nal::{self, nb, SocketAddr, IpAddr, Ipv4Addr};

#[derive(Copy, Clone, Debug)]
struct STACK;

pub struct TcpSocket {
    state: Option<std::net::TcpStream>,
}

impl TcpSocket {
    pub fn new() -> Self {
        Self {
            state: None,
        }
    }
}

fn to_nb(e: std::io::Error) -> nb::Error<std::io::Error> {
    use std::io::ErrorKind::{TimedOut, WouldBlock};

    match e.kind() {
        WouldBlock | TimedOut => nb::Error::WouldBlock,
        _ => e.into(),
    }
}

impl embedded_nal::TcpClientStack for STACK {
    type Error = std::io::Error;
    type TcpSocket = TcpSocket;

    fn socket(&mut self) -> Result<TcpSocket, Self::Error> {
        Ok(TcpSocket::new())
    }

    fn connect(&mut self, socket: &mut TcpSocket, remote: SocketAddr) -> nb::Result<(), Self::Error> {

        let octets: [u8; 4] = match remote.ip() {
            IpAddr::V4(addr) => addr.octets(),
                _ => panic!("Unsupported IP version"),
        };

        let soc = std::net::TcpStream::connect(
                std::net::SocketAddr::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::from(octets)),
                    remote.port()))?;
        soc.set_nonblocking(true)?;
        socket.state.replace(soc);

        Ok(())
    }

    fn is_connected(&mut self, socket: &TcpSocket) -> Result<bool, Self::Error> {
        Ok(socket.state.is_some())
    }

    fn send(&mut self, socket: &mut TcpSocket, buffer: &[u8]) -> nb::Result<usize, Self::Error> {
        let socket = socket.state.as_mut().unwrap();
        socket.write(buffer).map_err(to_nb)
    }

    fn receive(&mut self, socket: &mut TcpSocket, buffer: &mut [u8]) -> nb::Result<usize, Self::Error> {
        let socket = socket.state.as_mut().unwrap();
        socket.read(buffer).map_err(to_nb)
   }

    fn close(&mut self, socket: TcpSocket) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let stack = STACK.clone();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut client =
        MqttClient::<consts::U256, _>::new(localhost, "IntegrationTest", stack).unwrap();

    let mut published = false;
    let mut subscribed = false;

    loop {
        client
            .poll(|mut client, topic, payload, properties| {
                println!("{} < {}", topic, core::str::from_utf8(payload).unwrap());

                for property in properties {
                    match property {
                        Property::ResponseTopic(topic) => client
                            .publish(topic, "Pong".as_bytes(), QoS::AtMostOnce, &[])
                            .unwrap(),
                        _ => {}
                    };
                }

                if topic == "response" {
                    std::process::exit(0);
                }
            })
            .unwrap();

        if !subscribed {
            if client.is_connected().unwrap() {
                client.subscribe("response", &[]).unwrap();
                client.subscribe("request", &[]).unwrap();
                subscribed = true;
            }
        } else {
            if client.subscriptions_pending() == false {
                if !published {
                    println!("PUBLISH request");
                    let properties = [Property::ResponseTopic("response")];
                    client
                        .publish("request", "Ping".as_bytes(), QoS::AtMostOnce, &properties)
                        .unwrap();

                    published = true;
                }
            }
        }
    }
}
