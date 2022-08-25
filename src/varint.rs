use varint_rs::VarintReader;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Varint(pub u32);

impl From<u32> for Varint {
    fn from(val: u32) -> Varint {
        Varint(val)
    }
}

struct VarintVisitor;

struct VarintParser<'de, T: serde::de::SeqAccess<'de>> {
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
            _data: core::marker::PhantomData::default(),
        };
        Ok(Varint(reader.read_u32_varint()?))
    }
}

impl<'de> serde::de::Deserialize<'de> for Varint {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Varint, D::Error> {
        deserializer.deserialize_tuple(4, VarintVisitor)
    }
}
