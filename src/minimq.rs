// Minimal MQTT v5.0 client implementation

use crate::properties::{
    Data,
    property_data,
    SUBSCRIPTION_IDENTIFIER,
    RESPONSE_TOPIC,
    CORRELATION_DATA
};

use core::cmp::min;

// Maximum supported packet size
const PACKET_MAX: usize = 9000;

struct Packet {
    data: [u8; PACKET_MAX],
    start: usize,
    end: usize
}

impl Packet {

    fn new(start: usize, end: usize) -> Packet {
        assert!(start <= PACKET_MAX);
        assert!(end <= PACKET_MAX);
        assert!(start <= end);
        Packet {data: [0; PACKET_MAX], start: start, end: end}
    }

    fn push(&mut self, n: usize) {
        assert!(self.start >= n);
        self.start -= n;
    }

    fn pop(&mut self, n: usize) -> &[u8] {
        assert!(self.start + n <= self.end);
        let prev = self.start;
        self.start += n;
        &self.data[prev..self.start]
    }

    fn truncate(&mut self, n: usize) {
        assert!(self.start + n <= PACKET_MAX);
        self.end = self.start + n;
    }

    fn buf(&mut self) -> &mut [u8] {
        &mut self.data[self.start..self.end]
    }

    fn len(&self) -> usize {
        self.end - self.start
    }

    fn typ(&mut self) -> u8 {
        (self.buf()[0] >> 4) & 0x0F
    }

}


fn write_byte(b: u8, dst: &mut Packet) {
    dst.push(1);
    dst.buf()[0] = b;
}

fn write_two_byte_integer(n: u16, dst: &mut Packet) {
    dst.push(2);
    dst.buf()[0] = ((n >> 8) & 0xFF) as u8;
    dst.buf()[1] = ((n >> 0) & 0xFF) as u8;
}

fn write_four_byte_integer(n: u32, dst: &mut Packet){
    dst.push(4);
    dst.buf()[0] = ((n >> 24) & 0xFF) as u8;
    dst.buf()[1] = ((n >> 26) & 0xFF) as u8;
    dst.buf()[2] = ((n >> 8)  & 0xFF) as u8;
    dst.buf()[3] = ((n >> 0)  & 0xFF) as u8;
}

fn write_utf8_encoded_string(s: &[u8], dst: &mut Packet) {
    write_binary_data(s, dst);
}

fn write_variable_byte_integer(n: usize, dst: &mut Packet) {
    assert!(n <= 0x0FFF_FFFF); // 28 bits, up to four 7-bit fragments
    if n & (0b0111_1111 << 21) > 0 {
        write_byte((n >> 21) as u8 & 0b0111_1111, dst);
        write_byte((n >> 14) as u8 | 0b1000_0000, dst);
        write_byte((n >> 07) as u8 | 0b1000_0000, dst);
        write_byte((n >> 00) as u8 | 0b1000_0000, dst);
    } else if n & (0b0111_1111 << 14) > 0 {
        write_byte((n >> 14) as u8 & 0b0111_1111, dst);
        write_byte((n >> 07) as u8 | 0b1000_0000, dst);
        write_byte((n >> 00) as u8 | 0b1000_0000, dst);
    } else if n & (0b0111_1111 << 7) > 0 {
        write_byte((n >> 07) as u8 & 0b0111_1111, dst);
        write_byte((n >> 00) as u8 | 0b1000_0000, dst);
    } else {
        write_byte((n >> 00) as u8 & 0b0111_1111, dst);
    }
}

fn write_binary_data(b: &[u8], dst: &mut Packet) {
    dst.push(b.len());
    dst.buf()[..b.len()].copy_from_slice(b);
    write_two_byte_integer(b.len() as u16, dst);
}

fn write_fixed_header(typ: u8, flags: u8, dst: &mut Packet) {
    write_variable_byte_integer(dst.len(), dst);
    write_byte(((typ << 4) & 0xF0) | ((flags << 0) & 0x0F), dst);
}


fn read_byte(src: &mut Packet) -> Result<u8,()> {
    if src.len() < 1 { return Err(()) }
    Ok(src.pop(1)[0])
}

fn read_two_byte_integer(src: &mut Packet) -> Result<u16,()> {
    if src.len() < 2 { return Err(()) }
    let i = src.pop(2);
    Ok((i[0] as u16) << 8 |
       (i[1] as u16) << 0)
}

fn read_four_byte_integer(src: &mut Packet) -> Result<u32,()> {
    if src.len() < 4 { return Err(()) }
    let i = src.pop(4);
    Ok((i[0] as u32) << 24 |
       (i[1] as u32) << 16 |
       (i[2] as u32) <<  8 |
       (i[3] as u32) <<  0)
}

fn read_utf8_encoded_string(src: &mut Packet) -> Result<&[u8],()> {
    read_binary_data(src)
}

fn read_utf8_string_pair(src: &mut Packet) -> Result<(),()> {
    read_utf8_encoded_string(src)?;
    read_utf8_encoded_string(src)?;
    Ok(()) // XXX
}

fn read_variable_byte_integer(src: &mut Packet) -> Result<usize,()> {
    if let Some((i, b)) = parse_variable_byte_integer(src.buf()) {
        src.pop(b);
        return Ok(i)
    }
    Err(())
}

fn read_binary_data(src: &mut Packet) -> Result<&[u8],()> {
    let len = read_two_byte_integer(src)? as usize;
    if src.len() >= len {
        Ok(src.pop(len))
    } else {
        Err(())
    }
}

fn read_fixed_header(src: &mut Packet) -> Result<(u8, u8, usize),()> {
    let typ_flags = read_byte(src)?;
    let typ   = (typ_flags >> 4) & 0x0F;
    let flags = (typ_flags >> 0) & 0x0F;
    if let Ok(rlen) = read_variable_byte_integer(src) {
        Ok((typ, flags, rlen))
    } else {
        Err(())
    }
}


fn parse_variable_byte_integer(int: &[u8]) -> Option<(usize, usize)> {
    let len = if int.len() >= 1 && (int[0] & 0b1000_0000) == 0 { 1 }
         else if int.len() >= 2 && (int[1] & 0b1000_0000) == 0 { 2 }
         else if int.len() >= 3 && (int[2] & 0b1000_0000) == 0 { 3 }
         else if int.len() >= 4 && (int[3] & 0b1000_0000) == 0 { 4 }
         else { return None };
    let mut acc = 0;
    for i in 0..len { acc += ((int[i] & 0b0111_1111) as usize) << i*7; }
    Some((acc, len))
}


const CLIENT_ID_MAX: usize = 23;

const CONNECT: u8 = 1;
const CONNACK: u8 = 2;
fn msg_connect(flags: u8, keep_alive: u16, client_id: &[u8]) -> Packet {
    let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
    assert!(client_id.len() <= CLIENT_ID_MAX);
    write_binary_data(client_id, &mut p); // Payload
    write_variable_byte_integer(0, &mut p); // No properties
    write_two_byte_integer(keep_alive, &mut p);
    write_byte(flags & 0b11111110, &mut p); // Bit 0 reserved
    write_byte(5, &mut p); // Version 5
    write_utf8_encoded_string("MQTT".as_bytes(), &mut p);
    write_fixed_header(CONNECT, 0, &mut p);
    p
}

fn read_connack(p: &mut Packet) -> Result<u8,()> {
    let (typ, _, _) = read_fixed_header(p)?;
    if typ != CONNACK { return Err(()) }
    let _ = read_byte(p)?; // ACK flags
    let reason_code = read_byte(p)?;
    Ok(reason_code)
}


const PUBLISH: u8 = 3;

fn msg_publish(info: &PubInfo, payload: &[u8]) -> Packet {
    let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
    p.push(payload.len());
    p.buf().copy_from_slice(payload);
    if let Some(meta) = &info.cd {
        write_binary_data(meta.get(), &mut p);
        write_variable_byte_integer(CORRELATION_DATA, &mut p);
    }
    if let Some(meta) = &info.response {
        write_utf8_encoded_string(meta.get(), &mut p);
        write_variable_byte_integer(RESPONSE_TOPIC, &mut p);
    }
    write_variable_byte_integer(p.len() - payload.len(), &mut p); // Prop. len.
    write_utf8_encoded_string(info.topic.get(), &mut p);
    write_fixed_header(PUBLISH, 0, &mut p);
    p
}

fn read_publish(p: &mut Packet) -> Result<PubInfo,()> {
    let (typ, _, _) = read_fixed_header(p)?;
    if typ != PUBLISH { return Err(()) }
    let mut info = PubInfo::new();
    info.topic.set(read_utf8_encoded_string(p)?)?;
    let plen = read_variable_byte_integer(p)?;
    let payload = p.len() - plen;
    while p.len() > payload {
        match read_variable_byte_integer(p)? {
            SUBSCRIPTION_IDENTIFIER => {
                info.sid = Some(read_variable_byte_integer(p)?);
            }
            RESPONSE_TOPIC => {
                let mut response_topic = Meta::new(&[]);
                response_topic.set(read_utf8_encoded_string(p)?)?;
                info.response = Some(response_topic);
            }
            CORRELATION_DATA => {
                let mut cd = Meta::new(&[]);
                cd.set(read_binary_data(p)?)?;
                info.cd = Some(cd);
            }
            x => { skip_property(x, p)?; }
        }
    }
    if p.len() != payload { return Err(()) }
    Ok(info)
}


const SUBSCRIBE: u8 = 8;
const SUBACK: u8 = 9;

fn msg_subscribe(topic: &[u8], options: u8, id: u16, sid: usize) -> Packet {
    let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
    write_byte(options & 0b00111111, &mut p); // Bit 6-7 reserved
    write_utf8_encoded_string(topic, &mut p);
    let pend = p.len();
    write_variable_byte_integer(sid, &mut p);
    write_variable_byte_integer(SUBSCRIPTION_IDENTIFIER, &mut p);
    write_variable_byte_integer(p.len()-pend, &mut p); // Property length
    write_two_byte_integer(id, &mut p);
    write_fixed_header(SUBSCRIBE, 0b0010, &mut p); // [MQTT-3.8.1-1]
    p
}

fn read_suback(p: &mut Packet) -> Result<(u16, u8),()> {
    let (typ, _, _) = read_fixed_header(p)?;
    if typ != SUBACK { return Err(()) }
    let id = read_two_byte_integer(p)?;
    let plen = read_variable_byte_integer(p)?;
    if plen > p.len() { return Err(()) }
    p.pop(plen); // Discard properties.
    let reason_code = read_byte(p)?;
    Ok((id, reason_code))
}


pub struct PubInfo {
    pub sid: Option<usize>,
    pub topic: Meta,
    pub response: Option<Meta>,
    pub cd: Option<Meta>
}

impl PubInfo {
    pub fn new() -> PubInfo {
        PubInfo {sid: None, topic: Meta::new(&[]), response: None, cd: None}
    }
}


const META_MAX: usize = 64;

#[derive(Clone,Copy)]
pub struct Meta {
    buf: [u8; META_MAX],
    len: usize
}

impl Meta {

    pub fn new(data: &[u8]) -> Meta {
        let mut meta = Meta { buf: [0; META_MAX], len: data.len() };
        meta.set(data).unwrap();
        meta
    }

    pub fn get(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn set(&mut self, data: &[u8]) -> Result<(),()> {
        if data.len() <= META_MAX {
            self.len = data.len();
            self.buf[..self.len].copy_from_slice(data);
            Ok(())
        } else {
            Err(())
        }
    }

}


fn skip_property(property: usize, p: &mut Packet) -> Result<(),()>{
    match property_data(property) {
        Some(Data::Byte)                => { read_byte(p)?;                  }
        Some(Data::TwoByteInteger)      => { read_two_byte_integer(p)?;      }
        Some(Data::FourByteInteger)     => { read_four_byte_integer(p)?;     }
        Some(Data::VariableByteInteger) => { read_variable_byte_integer(p)?; }
        Some(Data::BinaryData)          => { read_binary_data(p)?;           }
        Some(Data::UTF8EncodedString)   => { read_utf8_encoded_string(p)?;   }
        Some(Data::UTF8StringPair)      => { read_utf8_string_pair(p)?;      }
        None => return Err(())
    }
    Ok(())
}


const FIXED_HEADER_MAX: usize = 5; // type/flags + remaining length

struct PacketReader {
    p: Packet,
    read: usize,
    have_fixed_header: bool
}

impl PacketReader {

    fn new() -> PacketReader {
        PacketReader {p: Packet::new(0, PACKET_MAX),
                      read: 0, have_fixed_header: false}
    }

    fn packet(&mut self) -> &mut Packet {
        &mut self.p
    }

    fn reset(&mut self) {
        self.p = Packet::new(0, PACKET_MAX);
        self.read = 0;
        self.have_fixed_header = false;
    }

    fn fill(&mut self, stream: &[u8]) -> usize {
        let read = min(stream.len(), self.p.len() - self.read);
        self.p.buf()[self.read..][..read].copy_from_slice(&stream[..read]);
        self.read += read;
        read
    }

    fn slurp(&mut self, stream: &[u8]) -> Result<(usize, bool), ()> {
        let prev_read = self.read;
        let read = self.fill(stream);
        if !self.have_fixed_header {
            if let Some(total_len) = self.probe_fixed_header() {
                if total_len > PACKET_MAX { return Err(()) }
                self.p.truncate(total_len);
                self.read = min(self.read, total_len);
                self.have_fixed_header = true;
            } else if self.read >= FIXED_HEADER_MAX { return Err(()) }
        }
        let read = min(read, self.read - prev_read);
        let complete = self.read == self.p.len();
        return Ok((read, complete))
    }

    fn probe_fixed_header(&mut self) -> Option<usize> {
        if self.read <= 1 { return None }
        match parse_variable_byte_integer(&self.p.buf()[..self.read][1..]) {
            Some((rlen, nbytes)) => Some(1 + nbytes + rlen),
            None => None
        }
    }

}


#[derive(PartialEq,Clone,Copy,Debug)]
pub enum ProtocolState {Close, Connect, Subscribe, Ready, Handle}

pub struct Protocol {
    p: Packet,
    pi: PubInfo,
    r: PacketReader,
    r_need_reset: bool,
    s: ProtocolState,
    pid: u16
}

impl Protocol {

    pub fn new() -> Protocol {
        Protocol { p: Packet::new(PACKET_MAX, PACKET_MAX),
                   pi: PubInfo::new(),
                   r: PacketReader::new(),
                   r_need_reset: false,
                   s: ProtocolState::Close,
                   pid: 0 }
    }

    pub fn state(&self) -> ProtocolState { self.s }

    pub fn connect(&mut self, client_id: &[u8], keep_alive: u16) -> &[u8] {
        assert!(self.s == ProtocolState::Close);
        self.p = msg_connect(
            0b10, // Clean Start
            keep_alive,
            &client_id[..CLIENT_ID_MAX]
        );
        self.s = ProtocolState::Connect;
        self.p.buf()
    }

    pub fn publish(&mut self, info: &PubInfo, payload: &[u8]) -> &[u8] {
        assert!(self.s == ProtocolState::Ready);
        self.p = msg_publish(info, payload);
        self.p.buf()
    }

    pub fn subscribe(&mut self, topic: &[u8], sid: usize) -> &[u8] {
        assert!(sid > 0 && sid <= 0x0F_FF_FF_FF); // (28 bits)
        assert!(self.s == ProtocolState::Ready);
        self.pid += 1;
        self.p = msg_subscribe(topic, 0, self.pid, sid); // XXX QoS
        self.s = ProtocolState::Subscribe;
        self.p.buf()
    }

    fn fsm(&mut self) -> Result<Option<&[u8]>,()> {
        let mut reply = None;
        let state = self.s;
        self.s = ProtocolState::Close;
        match state {
            ProtocolState::Close => return Err(()),
            ProtocolState::Connect => {
                let reason_code = read_connack(self.r.packet())?;
                if reason_code != 0 { return Err(()) }
                self.s = ProtocolState::Ready;
            }
            ProtocolState::Subscribe => {
                match self.r.packet().typ() {
                    PUBLISH => {
                        // Discard incoming publications while not Ready
                        read_publish(self.r.packet())?;
                        self.s = ProtocolState::Subscribe;
                    }
                    SUBACK => {
                        let (id, reason_code) = read_suback(self.r.packet())?;
                        if id != self.pid { return Err(()) }
                        if reason_code != 0 { return Err(()) }
                        self.s = ProtocolState::Ready;
                    }
                    _ => return Err(())
                }
            }
            ProtocolState::Ready => {
                self.pi = read_publish(self.r.packet())?;
                self.s = ProtocolState::Handle;
                reply = None // XXX - PUBACK
            }
            ProtocolState::Handle => return Err(()),
        }
        Ok(reply)
    }

    pub fn receive(&mut self, stream: &[u8]) -> Result<(usize,Option<&[u8]>),()> {
        if self.r_need_reset {
            self.r.reset();
            self.r_need_reset = false;
        } 
        match self.r.slurp(stream) {
            Ok((read, complete)) => {
                if !complete { return Ok((read,None)) }
                self.r_need_reset = true;
                let reply_or_err = self.fsm();
                match reply_or_err {
                    Ok(reply) => Ok((read,reply)),
                    Err(_) => Err(())
                }
            }
            Err(_) => {
                self.r.reset();
                Err(())
            }
        }
    }

    pub fn handle(&mut self) -> Option<(&PubInfo, &[u8])> {
        match self.s {
            ProtocolState::Handle => {
                self.s = ProtocolState::Ready;
                Some((&self.pi, self.r.packet().buf()))
            }
            _ => None
        }
    }

}


#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn protocol() {
        let mut proto = Protocol::new();
        let mut r = PacketReader::new();

        // Initial CONNECT
        let id = "01234567890123456789012".as_bytes();
        let connect = proto.connect(id, 10);
        let (read, done) = r.slurp(connect).unwrap();
        assert_eq!(read, connect.len());
        assert_eq!(done, true);
        assert_eq!(r.packet().typ(), CONNECT);
        assert_eq!(proto.state(), ProtocolState::Connect);
        r.reset();

        // Inbound PUBLISH during in Connect state is an error.
        let mut i = PubInfo::new();
        i.topic = Meta::new("test".as_bytes());
        let mut p = msg_publish(&i, "Hello, World!".as_bytes());
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(()));
        assert_eq!(proto.state(), ProtocolState::Close);
        proto.s = ProtocolState::Connect;

        // CONNACK with reason code > 0 is an error.
        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(42, &mut p); // Reason code
        write_byte(0, &mut p);  // Ack flags
        write_fixed_header(CONNACK, 0, &mut p);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(()));
        assert_eq!(proto.state(), ProtocolState::Close);
        proto.s = ProtocolState::Connect;

        // CONNACK with reason code 0 transitions to Ready state.
        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(0, &mut p); // Reason code
        write_byte(0, &mut p);  // Ack flags
        write_fixed_header(CONNACK, 0, &mut p);
        let (read, reply) = proto.receive(p.buf()).unwrap();
        assert_eq!(read, p.len());
        assert_eq!(reply, None);
        assert_eq!(proto.state(), ProtocolState::Ready);

        // Inbound messages other that PUBLISH during Ready state are errors.
        let mut p = msg_subscribe("test".as_bytes(), 0, 42, 12);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(()));
        assert_eq!(proto.state(), ProtocolState::Close);
        proto.s = ProtocolState::Ready;

        // Inbound PUBLISH during Ready state need to be handled.
        let payload = "Hello, World!".as_bytes();
        let mut i = PubInfo::new();
        i.response = Some(Meta::new("test".as_bytes()));
        i.cd = Some(Meta::new("foo".as_bytes()));
        let mut p = msg_publish(&i, payload);
        p.pop(5); // Pop prop. len., emtpy topic, fixed header
        // Graft Subscription Identifier into packet...
        write_variable_byte_integer(12, &mut p);
        write_variable_byte_integer(SUBSCRIPTION_IDENTIFIER, &mut p);
        write_variable_byte_integer(p.len() - payload.len(), &mut p); // Prop. len.
        write_utf8_encoded_string(i.topic.get(), &mut p);
        write_fixed_header(PUBLISH, 0, &mut p);
        let (read, reply) = proto.receive(p.buf()).unwrap();
        assert_eq!(read, p.len());
        assert_eq!(reply, None); // XXX QoS / PUBACK
        if let Some((pi, pl)) = proto.handle() {
            assert_eq!(pi.sid, Some(12));
            assert_eq!(pi.response.as_ref().unwrap().get(), "test".as_bytes());
            assert_eq!(pi.cd.as_ref().unwrap().get(), "foo".as_bytes());
            i.topic.set(pi.response.as_ref().unwrap().get()).unwrap();
            assert_eq!(pl, payload);
            let response = proto.publish(&i, "OK".as_bytes());
            let (read, done) = r.slurp(response).unwrap();
            assert_eq!(read, response.len());
            assert_eq!(done, true);
            r.reset();
            assert_eq!(proto.state(), ProtocolState::Ready);
        } else {
            panic!("Expected PUBLISH to handle.");
        }

        // We can SUBSCRIBE while in Ready state.
        let sub = proto.subscribe("test".as_bytes(), 12);
        let (read, done) = r.slurp(sub).unwrap();
        assert_eq!(read, sub.len());
        assert_eq!(done, true);
        assert_eq!(r.packet().typ(), SUBSCRIBE);
        assert_eq!(proto.state(), ProtocolState::Subscribe);
        r.reset();

        // Inbound PUBLISH messages are discarded while in Subscribe state.
        let mut i = PubInfo::new();
        i.topic = Meta::new("test".as_bytes());
        let mut p = msg_publish(&i, "Hello, World!".as_bytes());
        let (read, reply) = proto.receive(p.buf()).unwrap();
        assert_eq!(read, p.len());
        assert_eq!(reply, None);
        assert_eq!(proto.state(), ProtocolState::Subscribe);

        // Matching inbound SUBACK transitions from Subscribe into Ready state.
        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(0, &mut p); // Reason code
        write_variable_byte_integer(0, &mut p); // No properties
        write_two_byte_integer(proto.pid, &mut p); // Id
        write_fixed_header(SUBACK, 0, &mut p);
        let (read, reply) = proto.receive(p.buf()).unwrap();
        assert_eq!(read, p.len());
        assert_eq!(reply, None);
        assert_eq!(proto.state(), ProtocolState::Ready);

        // Any other inbound message while in Subscribe state is an error.
        // Reason Code > 0
        proto.s = ProtocolState::Subscribe;
        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(1, &mut p); // Reason code
        write_variable_byte_integer(0, &mut p); // No properties
        write_two_byte_integer(proto.pid, &mut p); // Id
        write_fixed_header(SUBACK, 0, &mut p);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(()));
        assert_eq!(proto.state(), ProtocolState::Close);
        // Wrong Packet ID
        proto.s = ProtocolState::Subscribe;
        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(1, &mut p); // Reason code
        write_variable_byte_integer(0, &mut p); // No properties
        write_two_byte_integer(0, &mut p); // Id
        write_fixed_header(SUBACK, 0, &mut p);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(()));
        assert_eq!(proto.state(), ProtocolState::Close);
        // Bogus message
        proto.s = ProtocolState::Subscribe;
        let mut p = msg_subscribe("test".as_bytes(), 0, 42, 12);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(()));
        assert_eq!(proto.state(), ProtocolState::Close);
    }

    #[test]
    fn connect() {
        let mut p = msg_connect(0b10, 10, "foobar42".as_bytes());
        assert_eq!(p.typ(), CONNECT);
        let (typ, flags, rlen) = read_fixed_header(&mut p).unwrap();
        assert_eq!(typ, CONNECT);
        assert_eq!(flags, 0);
        assert_eq!(rlen, p.len());
        let magic = read_utf8_encoded_string(&mut p).unwrap();
        assert_eq!(magic, "MQTT".as_bytes());
        let version = read_byte(&mut p).unwrap();
        assert_eq!(version, 5);
        let flags = read_byte(&mut p).unwrap();
        assert_eq!(flags, 0b10);
        let keep_alive = read_two_byte_integer(&mut p).unwrap();
        assert_eq!(keep_alive, 10);
        let prop_len = read_variable_byte_integer(&mut p).unwrap();
        assert_eq!(prop_len, 0);
        let client_id = read_binary_data(&mut p).unwrap();
        assert_eq!(client_id, "foobar42".as_bytes());

        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(42, &mut p); // Reason code
        write_byte(0, &mut p);  // Ack flags
        write_fixed_header(CONNACK, 0, &mut p);
        let reason_code = read_connack(&mut p).unwrap();
        assert_eq!(reason_code, 42);
    }

    #[test]
    fn publish() {
        let mut i = PubInfo::new();
        i.topic = Meta::new("test".as_bytes());
        i.cd = Some(Meta::new("foobar".as_bytes()));
        let mut p = msg_publish(&i, "Hello, World!".as_bytes());
        let i = read_publish(&mut p).unwrap();
        assert_eq!(i.sid, None);
        assert_eq!(i.response.is_none(), true);
        assert_eq!(i.cd.unwrap().get(), "foobar".as_bytes());
        assert_eq!(p.buf(), "Hello, World!".as_bytes());
    }

    #[test]
    fn subscribe() {
        let mut p = msg_subscribe("test".as_bytes(), 0b11111111, 42, 12);
        let (typ, flags, rlen) = read_fixed_header(&mut p).unwrap();
        assert_eq!(typ, SUBSCRIBE);
        assert_eq!(flags, 0b0010);
        assert_eq!(rlen, p.len());
        let id = read_two_byte_integer(&mut p).unwrap();
        assert_eq!(id, 42);
        let plen = read_variable_byte_integer(&mut p).unwrap();
        assert_eq!(plen, 2);
        let prop = read_variable_byte_integer(&mut p).unwrap();
        assert_eq!(prop, SUBSCRIPTION_IDENTIFIER);
        let sid = read_variable_byte_integer(&mut p).unwrap();
        assert_eq!(sid, 12);
        let topic = read_utf8_encoded_string(&mut p).unwrap();
        assert_eq!(topic, "test".as_bytes());
        let options = read_byte(&mut p).unwrap();
        assert_eq!(options, 0b00111111);

        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(1, &mut p); // Reason code
        p.push(13);
        write_variable_byte_integer(13, &mut p); // Bogus properties
        write_two_byte_integer(42, &mut p); // Id
        write_fixed_header(SUBACK, 0, &mut p);
        let (id, reason_code) = read_suback(&mut p).unwrap();
        assert_eq!(id, 42);
        assert_eq!(reason_code, 1);
    }

    #[test]
    fn slurp() {
        let mut r = PacketReader::new();
        let (read, done) = r.slurp(&[0b1000_0001]).unwrap();
        assert_eq!(read, 1);
        assert_eq!(done, false);
        let (read, done) = r.slurp(&[2, 1, 2, 3, 4, 5]).unwrap();
        assert_eq!(read, 3);
        assert_eq!(done, true);
        assert_eq!(r.p.typ(), 0b1000);
        assert_eq!(r.p.len(), 4);
        assert_eq!(r.p.buf()[0], 0b1000_0001);
        assert_eq!(r.p.buf()[1], 2);
        assert_eq!(r.p.buf()[2], 1);
        assert_eq!(r.p.buf()[3], 2);
        let (read, done) = r.slurp(&[6]).unwrap();
        assert_eq!(read, 0);
        assert_eq!(done, true);
        r.reset();
        let (read, done) = r.slurp(&[0, 0b1000_0000, 0b1000_0000]).unwrap();
        assert_eq!(read, 3);
        assert_eq!(done, false);
        let (read, done) = r.slurp(&[0b1000_0000]).unwrap();
        assert_eq!(read, 1);
        assert_eq!(done, false);
        let err = r.slurp(&[0b1000_0000]);
        assert_eq!(err, Err(()));
        r.reset();
        let err = r.slurp(&[0, 0b1111_1111, 0b0111_1111]);
        assert_eq!(err, Err(()));
    }

    #[test]
    fn property() {
        let data = property_data(SUBSCRIPTION_IDENTIFIER);
        assert_eq!(data.unwrap(), Data::VariableByteInteger);
        let none = property_data(0);
        assert_eq!(none, None);
    }

}
