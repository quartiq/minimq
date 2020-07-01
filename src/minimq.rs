// Minimal MQTT v5.0 client implementation

use enum_iterator::IntoEnumIterator;

use crate::{
    packet_writer::PacketWriter,
    packet_reader::PacketReader,
    serialize::{self, integer_size},
    deserialize::{self, ReceivedPacket},
};

use crate::properties;

const CLIENT_ID_MAX: usize = 23;

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Error {
    Bounds,
    DataSize,
    Invalid,
    PacketSize,
    EmptyPacket,
    Failed,
    PartialPacket,
    InvalidState,
    MalformedPacket,
    MalformedInteger,
    UnknownProperty,
    UnsupportedPacket,
}

#[derive(IntoEnumIterator, Copy, Clone, PartialEq, Debug)]
pub enum MessageType {
    Invalid = -1,

    Reserved = 0,
    Connect = 1,
    ConnAck = 2,
    Publish = 3,
    PubAck = 4,
    PubRec = 5,
    PubRel = 6,
    PubComp = 7,
    Subscribe = 8,
    SubAck = 9,
    Unsubscribe = 10,
    UnsubAck = 11,
    PingReq = 12,
    PingResp = 13,
    Disconnect = 14,
    Auth = 15,
}

impl From<u8> for MessageType {
    fn from(val: u8) -> Self {
        for entry in Self::into_enum_iter() {
            if entry as u8 == val {
                return entry;
            }
        }

        return MessageType::Invalid;
    }
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

    pub fn variable_header_length(&self) -> usize {

        // Include the length of the mandatory topic name field in the variable header.
        let mut length = self.topic.get().len() + 2;

        // TODO: Handle sender ID for QoS 1 or 2.

        let property_length = self.get_property_length();

        // Include length of the properties field.
        length += property_length + integer_size(property_length);

        length
    }

    fn get_property_length(&self) -> usize {
        let mut property_length = 0;
        if let Some(response) = &self.response {
            // The length of this entry is 2 bytes for the string length encoding and the string
            // data.
            property_length += integer_size(properties::RESPONSE_TOPIC);
            property_length += response.get().len() + 2;
        }

        if let Some(cd) = &self.cd {
            property_length += integer_size(properties::CORRELATION_DATA);
            property_length += cd.get().len() + 2;
        }

        property_length
    }

    pub fn write_variable_header(&self, packet: &mut PacketWriter) -> Result<(), Error> {

        // Write the topic name.
        packet.write_binary_data(self.topic.get())?;

        // TODO: Handle the sender ID.

        // Write the length of the properties list.
        packet.write_variable_length_integer(self.get_property_length())?;

        // Write the response topic property.
        if let Some(meta) = &self.response {
            packet.write_variable_length_integer(properties::RESPONSE_TOPIC)?;
            packet.write_binary_data(meta.get())?;
        }

        // Write the correlation data.
        if let Some(meta) = &self.cd {
            packet.write_variable_length_integer(properties::CORRELATION_DATA)?;
            packet.write_binary_data(meta.get())?;
        }

        // TODO: Handle additional properties.

        Ok(())
    }
}

const META_MAX: usize = 64;

#[derive(Clone, Copy)]
pub struct Meta {
    pub buf: [u8; META_MAX],
    pub len: usize
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

#[derive(PartialEq,Clone,Copy,Debug)]
pub enum ProtocolState {
    Close,
    Init,
    Connect,
    Subscribe,
    Ready,
    Handle,
}

pub struct Protocol<'a> {
    pi: PubInfo,
    packet_reader: PacketReader<'a>,
    state: ProtocolState,
    pid: u16
}

impl<'a> Protocol<'a> {

    pub fn new(rx_buffer: &'a mut [u8]) -> Protocol {
        Protocol {
            pi: PubInfo::new(),
            packet_reader: PacketReader::new(rx_buffer),
            state: ProtocolState::Init,

            // Only non-zero packet identifiers are allowed.
            pid: 1
        }
    }

    pub fn state(&self) -> ProtocolState {
        self.state
    }

    pub fn connect(&mut self, dest: &mut [u8], client_id: &[u8], keep_alive: u16) -> Result<usize, Error> {
        if self.state != ProtocolState::Init {
            return Err(Error::InvalidState);
        }

        self.state = ProtocolState::Connect;
        serialize::connect_message(dest, client_id, keep_alive)
    }

    pub fn reset(&mut self) {
        self.state = ProtocolState::Init;
        self.packet_reader.reset();
        self.pid = 1;
    }

    pub fn publish(&self, dest: &mut [u8], info: &PubInfo, payload: &[u8]) -> Result<usize, Error> {
        if self.state != ProtocolState::Ready {
            return Err(Error::InvalidState);
        }

        serialize::publish_message(dest, info, payload)
    }

    pub fn subscribe<'b>(&mut self, dest: &mut [u8], topic: &'b str, sid: usize) -> Result<usize, Error> {
        if self.state != ProtocolState::Ready {
            return Err(Error::InvalidState);
        }

        self.state = ProtocolState::Subscribe;

        let size = serialize::subscribe_message(dest, topic, sid, self.pid)?;

        Ok(size)
    }

    pub fn receive(&mut self, stream: &[u8]) -> Result<usize, Error> {
        match self.packet_reader.slurp(stream) {
            Err(error) => {
                // If we got a generic error, reset the packet reader.
                self.packet_reader.reset();

                // TODO: We should only forward the error if we can't recover from it.
                Err(error)
            },
            x => x,
        }
    }

    pub fn handle<F>(&mut self, data_handler: F) -> Result<(), Error>
    where
        F: FnOnce(&Self, &PubInfo, &[u8])
    {
        // If there is a packet available for processing, handle it now, potentially updating our
        // internal state.
        if self.packet_reader.packet_available() {
            if let Some(publish_info) = self.state_machine()? {
                // Call a handler function to deal with the received data.
                let payload = self.packet_reader.payload()?;
                data_handler(&self, &publish_info, payload);
            }

            self.packet_reader.pop_packet()?;
        }

        Ok(())
    }


    fn increment_packet_identifier(&mut self) {
        let (result, overflow) = self.pid.overflowing_add(1);

        // Packet identifiers must always be non-zero.
        if overflow {
            self.pid = 1;
        } else {
            self.pid = result;
        }
    }

    fn state_machine(&mut self) -> Result<Option<PubInfo>, Error> {
        let state = self.state;
        self.state = ProtocolState::Close;

        let packet = deserialize::parse_message(&mut self.packet_reader)?;

        match state {
            ProtocolState::Connect => {
                if let ReceivedPacket::ConnAck(acknowledge) = packet {
                    if acknowledge.reason_code != 0 {
                        return Err(Error::Failed);
                    }

                    self.state = ProtocolState::Ready;
                    Ok(None)
                } else {
                    // TODO: Handle something other than a connect acknowledge?
                    Err(Error::Invalid)
                }
            },

            ProtocolState::Subscribe => {
                match packet {
                    ReceivedPacket::Publish(_) => {
                        // Discard incoming publications while not Ready
                        self.state = ProtocolState::Subscribe;
                        Ok(None)
                    },
                    ReceivedPacket::SubAck(subscribe_acknowledge) => {
                        if subscribe_acknowledge.packet_identifier != self.pid {
                            return Err(Error::Invalid)
                        }

                        if subscribe_acknowledge.reason_code != 0 {
                            return Err(Error::Failed)
                        }

                        self.increment_packet_identifier();

                        self.state = ProtocolState::Ready;
                        Ok(None)
                    },
                    _ => Err(Error::UnsupportedPacket),
                }
            },

            ProtocolState::Ready => {
                if let ReceivedPacket::Publish(publish_info) = packet {
                    // TODO: Send a PUBACK
                    self.state = ProtocolState::Ready;
                    Ok(Some(publish_info))
                } else {
                    Err(Error::UnsupportedPacket)
                }
            },
            _ => Err(Error::InvalidState)
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn protocol() {
        let mut protocol_buffer = [0u8; 900];
        let mut proto = Protocol::new(&mut protocol_buffer);
        let mut buffer = [0u8; 900];

        // Initial CONNECT
        let id = "01234567890123456789012".as_bytes();
        let _ = proto.connect(&mut buffer, id, 10).unwrap();
        assert_eq!(proto.state(), ProtocolState::Connect);

        // Inbound PUBLISH while in Connect state is an error.
        let mut info = PubInfo::new();
        info.topic = Meta::new("test".as_bytes());
        let len = serialize::publish_message(&mut buffer, &info, "Hello, World".as_bytes()).unwrap();

        let receive_length = proto.receive(&buffer[..len]).unwrap();
        assert_eq!(len, receive_length);

        let err = proto.handle(|_mqtt, _info, _payload| {});
        assert_eq!(err, Err(Error::Invalid));
        assert_eq!(proto.state(), ProtocolState::Close);
        proto.state = ProtocolState::Connect;

        // TODO: Refactor the following unit tests.

        /*
        // Inbound messages other that PUBLISH during Ready state are errors.
        let mut p = msg_subscribe("test".as_bytes(), 0, 42, 12);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(()));
        assert_eq!(proto.state(), ProtocolState::Close);
        proto.state = ProtocolState::Ready;

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
            let mut response: [u8; 9000] = [0; 9000];
            let len = proto.publish(&mut response, &i, "OK".as_bytes()).unwrap();
            let (read, done) = r.slurp(&response[..len]).unwrap();
            assert_eq!(read, len);
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
        proto.state = ProtocolState::Subscribe;
        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(1, &mut p); // Reason code
        write_variable_byte_integer(0, &mut p); // No properties
        write_two_byte_integer(proto.pid, &mut p); // Id
        write_fixed_header(SUBACK, 0, &mut p);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(Error::Invalid));
        assert_eq!(proto.state(), ProtocolState::Close);
        // Wrong Packet ID
        proto.state = ProtocolState::Subscribe;
        let mut p = Packet::new(PACKET_MAX, PACKET_MAX);
        write_byte(1, &mut p); // Reason code
        write_variable_byte_integer(0, &mut p); // No properties
        write_two_byte_integer(0, &mut p); // Id
        write_fixed_header(SUBACK, 0, &mut p);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(Error::Invalid));
        assert_eq!(proto.state(), ProtocolState::Close);
        // Bogus message
        proto.state = ProtocolState::Subscribe;
        let mut p = msg_subscribe("test".as_bytes(), 0, 42, 12);
        let err = proto.receive(p.buf());
        assert_eq!(err, Err(Error::Invalid));
        assert_eq!(proto.state(), ProtocolState::Close);
        */
    }

    #[test]
    fn property() {
        let data = properties::property_data(properties::SUBSCRIPTION_IDENTIFIER);
        assert_eq!(data.unwrap(), properties::Data::VariableByteInteger);
        let none = properties::property_data(0);
        assert_eq!(none, None);
    }
}
