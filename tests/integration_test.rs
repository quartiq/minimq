use minimq::mqtt_client::{MqttClient, QoS, Property, consts};

use std::io::{self, Write, Read};
use std::net::{self, TcpStream};
use std::cell::RefCell;
use nb;

use embedded_nal::{self, IpAddr, Ipv4Addr, SocketAddr};

struct StandardStack {
    stream: RefCell<Option<TcpStream>>,
    mode: RefCell<embedded_nal::Mode>,
}

impl StandardStack {
    pub fn new() -> StandardStack {
        StandardStack {
            stream: RefCell::new(None),
            mode: RefCell::new(embedded_nal::Mode::Blocking)
        }
    }
}

impl embedded_nal::TcpStack for StandardStack {

    type Error = io::Error;

    type TcpSocket = ();

    fn open(&self, mode: embedded_nal::Mode) -> Result<Self::TcpSocket, Self::Error> {
        self.mode.replace(mode);
        Ok(())
    }

    fn connect(&self, _socket: Self::TcpSocket, remote: SocketAddr) -> Result<Self::TcpSocket, Self::Error> {
        let ip = match remote.ip() {
            IpAddr::V4(addr) => net::IpAddr::V4(net::Ipv4Addr::new(addr.octets()[0],
                                                                   addr.octets()[1],
                                                                   addr.octets()[2],
                                                                   addr.octets()[3])),
            IpAddr::V6(addr) => net::IpAddr::V6(net::Ipv6Addr::new(addr.segments()[0],
                                                                   addr.segments()[1],
                                                                   addr.segments()[2],
                                                                   addr.segments()[3],
                                                                   addr.segments()[4],
                                                                   addr.segments()[5],
                                                                   addr.segments()[6],
                                                                   addr.segments()[7])),
        };

        let remote = net::SocketAddr::new(ip, remote.port());

        let stream = TcpStream::connect(remote).unwrap();

        match *self.mode.borrow() {
            embedded_nal::Mode::NonBlocking => stream.set_nonblocking(true)?,
            embedded_nal::Mode::Blocking => stream.set_nonblocking(false)?,
            embedded_nal::Mode::Timeout(t) => {
                stream.set_read_timeout(Some(std::time::Duration::from_secs(t.into())))?;
                stream.set_write_timeout(Some(std::time::Duration::from_secs(t.into())))?;
            }
        }
        self.stream.replace(Some(stream));

        Ok(())
    }

    fn is_connected(&self, _socket: &Self::TcpSocket) -> Result<bool, Self::Error> {
        Ok(self.stream.borrow().is_some())
    }

    fn write(&self, _socket: &mut Self::TcpSocket, buffer: &[u8]) -> nb::Result<usize, Self::Error> {
        match &mut *self.stream.borrow_mut() {
            Some(stream) => {
                match stream.write(buffer) {
                    Ok(len) => Ok(len),
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            Err(nb::Error::WouldBlock)
                        } else {
                            Err(nb::Error::Other(e))
                        }
                    }
                }
            },
            None => Ok(0),
        }
    }

    fn read(&self, _socket: &mut Self::TcpSocket, buffer: &mut [u8]) -> nb::Result<usize, Self::Error> {
        match &mut *self.stream.borrow_mut() {
            Some(stream) => {
                match stream.read(buffer) {
                    Ok(len) => Ok(len),
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            Err(nb::Error::WouldBlock)
                        } else {
                            Err(nb::Error::Other(e))
                        }
                    }
                }
            },
            None => Ok(0),
        }
    }

    fn close(&self, _socket: Self::TcpSocket) -> Result<(), Self::Error> {
        self.stream.replace(None).unwrap();

        Ok(())
    }
}

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let stack = StandardStack::new();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut client = MqttClient::<_, consts::U256>::new(localhost, "IntegrationTest", stack).unwrap();

    let mut published = false;
    client.subscribe("response", &[]).unwrap();
    client.subscribe("request", &[]).unwrap();

    loop {

        client.poll(|mqtt, req, payload| {
            if req.cd.is_some() {
                println!(
                    "{}:{} < {} [cd:{}]",
                    req.sid.unwrap_or(0),
                    core::str::from_utf8(req.topic.get()).unwrap(),
                    core::str::from_utf8(payload).unwrap(),
                    core::str::from_utf8(req.cd.unwrap().get()).unwrap()
                );
            } else {
                println!(
                    "{}:{} < {}",
                    req.sid.unwrap_or(0),
                    core::str::from_utf8(req.topic.get()).unwrap(),
                    core::str::from_utf8(payload).unwrap(),
                );
            }

            match req.response {
                Some(topic) => mqtt.publish(core::str::from_utf8(topic.get()).unwrap(),
                                            "Pong".as_bytes(),
                                            QoS::AtMostOnce,
                                            &[]).unwrap(),
                None => std::process::exit(0),
            };
        })
        .unwrap();

        if client.subscriptions_pending() == false {
            if !published {
                println!("PUBLISH request");
                let properties = [Property::ResponseTopic("response")];
                client.publish("request", "Ping".as_bytes(), QoS::AtMostOnce, &properties).unwrap();

                published = true;
            }
        }
    }
}
