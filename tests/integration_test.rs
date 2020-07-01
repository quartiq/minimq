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
    let mut buffer = [0u8; 900];
    let mut mqtt = minimq::Protocol::new(&mut buffer);

    println!("Connecting to MQTT broker at 127.0.0.1:1883");
    let mut stream = connect("127.0.0.1:1883")?;

    println!("Sending CONNECT");
    let id = "01234567890123456789012".as_bytes();
    let mut packet_buffer = [0u8; 900];
    let len = mqtt.connect(&mut packet_buffer, id, 10).unwrap();
    write(&mut stream, &packet_buffer[..len])?;

    let mut buffer: [u8; 9000] = [0; 9000];
    let (sub_req, sub_res) = (1, 2);
    let (mut subscribed_req, mut subscribed_res) = (false, false);
    let mut published = false;
    loop {
        let mut buf = [0; 1024];
        let received = read(&mut stream, &mut buf)?;
        let mut processed = 0;
        while processed < received {
            let read = mqtt.receive(&buf[processed..received]).unwrap();
            processed += read;

            mqtt.handle(|mqtt, req, payload| {
                println!("{}:{} < {} [cd:{}]",
                         req.sid.unwrap_or(0),
                         str(req.topic.get()),
                         str(payload),
                         str(req.cd.unwrap().get()));
                if req.sid == Some(sub_req) {
                    let mut res = minimq::PubInfo::new();
                    res.topic = req.response.unwrap();
                    res.cd = req.cd;
                    let len = mqtt.publish(&mut buffer, &res, "Ping".as_bytes()).unwrap();
                    write(&mut stream, &buffer[..len]).unwrap();
                } else {
                    std::process::exit(0);
                }
            }).unwrap();

            if subscribed_req && subscribed_res {
                if !published && mqtt.state() == minimq::ProtocolState::Ready {
                    println!("PUBLISH request");
                    let mut req = minimq::PubInfo::new();
                    req.topic = minimq::Meta::new("request".as_bytes());
                    req.response = Some(minimq::Meta::new("response".as_bytes()));
                    req.cd = Some(minimq::Meta::new("foo".as_bytes()));
                    let len = mqtt.publish(&mut buffer, &req, "Ping".as_bytes()).unwrap();
                    write(&mut stream, &buffer[..len])?;
                    published = true;
                }
            }

            if !subscribed_req && mqtt.state() == minimq::ProtocolState::Ready {
                println!("SUBSCRIBE request");
                let len = mqtt.subscribe(&mut packet_buffer, "request", sub_req).unwrap();
                write(&mut stream, &packet_buffer[..len])?;
                subscribed_req = true;
            }
            if !subscribed_res && mqtt.state() == minimq::ProtocolState::Ready {
                println!("SUBSCRIBE response");
                let len = mqtt.subscribe(&mut packet_buffer, "response", sub_res).unwrap();
                write(&mut stream, &packet_buffer[..len])?;
                subscribed_res = true;
            }
        }
    }
}
