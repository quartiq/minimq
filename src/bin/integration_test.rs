use minimq;

use std::io;
use std::io::prelude::*;
use std::net::TcpStream;

fn connect(addr: &str) -> io::Result<TcpStream> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nonblocking(true)?;
    Ok(stream)
}

fn read(stream: &mut TcpStream, data: &mut [u8]) -> io::Result<usize> {
    loop {
        match stream.read(data) {
            Ok(read) => { return Ok(read); }
            _ => ()
        }
    }
}

fn write(stream: &mut TcpStream, data: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < data.len()  {
        match stream.write(&data[written..]) {
            Ok(n) => { written += n; }
            _ => ()
        }
    }
    Ok(())
}

fn str(b: &[u8]) -> String { String::from_utf8(b.to_vec()).unwrap() }

#[test]
fn main() -> std::io::Result<()> {
    let mut mqtt = minimq::Protocol::new();

    println!("Connecting to MQTT broker at 127.0.0.1:1883");
    let mut stream = connect("127.0.0.1:1883")?;

    println!("Sending CONNECT");
    let id = "01234567890123456789012".as_bytes();
    write(&mut stream, mqtt.connect(id, 10))?;

    let (sub_req, sub_res) = (1, 2);
    let (mut subscribed_req, mut subscribed_res) = (false, false);
    let mut published = false;
    loop {
        let mut buf = [0; 1024];
        let received = read(&mut stream, &mut buf)?;
        let mut processed = 0;
        while processed < received {
            let (read, reply) = mqtt.receive(&buf[processed..]).unwrap();
            processed += read;

            if let Some(reply) = reply {
                println!("Sending reply");
                write(&mut stream, reply)?;
            }

            if let Some((req, payload)) = mqtt.handle() {
                println!("{}:{} < {} [cd:{}]",
                         req.sid.unwrap_or(0),
                         str(req.topic.get()),
                         str(payload),
                         str(req.cd.unwrap().get()));
                if req.sid == Some(sub_req) {
                    let mut res = minimq::PubInfo::new();
                    res.topic = req.response.unwrap();
                    res.cd = req.cd;
                    write(&mut stream, mqtt.publish(&res, "Pong".as_bytes()))?;
                } else {
                    std::process::exit(0);
                }
            }

            if subscribed_req && subscribed_res {
                if !published && mqtt.state() == minimq::ProtocolState::Ready {
                    println!("PUBLISH request");
                    let mut req = minimq::PubInfo::new();
                    req.topic = minimq::Meta::new("request".as_bytes());
                    req.response = Some(minimq::Meta::new("response".as_bytes()));
                    req.cd = Some(minimq::Meta::new("foo".as_bytes()));
                    write(&mut stream, mqtt.publish(&req, "Ping".as_bytes()))?;
                    published = true;
                }
            }

            if !subscribed_req && mqtt.state() == minimq::ProtocolState::Ready {
                println!("SUBSCRIBE request");
                write(&mut stream, mqtt.subscribe("request".as_bytes(), sub_req))?;
                subscribed_req = true;
            }
            if !subscribed_res && mqtt.state() == minimq::ProtocolState::Ready {
                println!("SUBSCRIBE response");
                write(&mut stream, mqtt.subscribe("response".as_bytes(), sub_res))?;
                subscribed_res = true;
            }
        }
    }
}
