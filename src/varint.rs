use heapless::Vec;
use serde::ser::SerializeSeq;
use varint_rs::{VarintReader, VarintWriter};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Varint(pub u32);

impl Varint {
    pub fn len(&self) -> usize {
        let mut buffer = VarintBuffer::new();
        buffer.write_u32_varint(self.0).unwrap();
        buffer.data.len()
    }
}

impl From<u32> for Varint {
    fn from(val: u32) -> Varint {
        Varint(val)
    }
}

pub struct VarintBuffer {
    pub data: Vec<u8, 4>,
}

impl VarintBuffer {
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }
}

impl VarintWriter for VarintBuffer {
    type Error = ();
    fn write(&mut self, byte: u8) -> Result<(), ()> {
        self.data.push(byte).map_err(|_| ())
    }
}

struct VarintVisitor;

struct VarintParser<'de, T> {
    seq: T,
    _data: core::marker::PhantomData<&'de ()>,
}

impl<'de, T: serde::de::SeqAccess<'de>> VarintReader for VarintParser<'de, T> {
    type Error = T::Error;

    fn read(&mut self) -> Result<u8, T::Error> {
        use serde::de::Error;
        let next = self.seq.next_element()?;
        next.ok_or_else(|| T::Error::custom("Invalid varint"))
    }
}

impl<'de> serde::de::Visitor<'de> for VarintVisitor {
    type Value = Varint;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "Varint")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        let mut reader = VarintParser {
            seq,
            _data: core::marker::PhantomData,
        };
        Ok(Varint(reader.read_u32_varint()?))
    }
}

impl<'de> serde::de::Deserialize<'de> for Varint {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Varint, D::Error> {
        deserializer.deserialize_tuple(4, VarintVisitor)
    }
}

impl serde::Serialize for Varint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        let mut buffer = VarintBuffer::new();
        buffer
            .write_u32_varint(self.0)
            .map_err(|_| S::Error::custom("Failed to encode varint"))?;

        let mut seq = serializer.serialize_seq(Some(buffer.data.len()))?;
        for byte in buffer.data.iter() {
            seq.serialize_element(byte)?;
        }
        seq.end()
    }
}
