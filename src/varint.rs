use serde::ser::SerializeSeq;

pub(crate) const MQTT_VARINT_MAX: u32 = 0x0FFF_FFFF;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Varint(pub u32);

impl Varint {
    pub fn len(&self) -> usize {
        match self.0 {
            0..=0x7F => 1,
            0x80..=0x3FFF => 2,
            0x4000..=0x1F_FFFF => 3,
            _ => 4,
        }
    }
}

impl From<u32> for Varint {
    fn from(val: u32) -> Varint {
        Varint(val)
    }
}

pub struct VarintBuffer {
    data: [u8; 4],
    len: u8,
}

impl VarintBuffer {
    #[inline]
    pub const fn new() -> Self {
        Self {
            data: [0; 4],
            len: 0,
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data[..usize::from(self.len)]
    }

    #[inline]
    fn push(&mut self, byte: u8) -> Result<(), ()> {
        let index = usize::from(self.len);
        let slot = self.data.get_mut(index).ok_or(())?;
        *slot = byte;
        self.len += 1;
        Ok(())
    }
}

struct VarintVisitor;

#[inline]
pub(crate) fn write_mqtt_u32_varint(mut value: u32, out: &mut VarintBuffer) -> Result<(), ()> {
    if value > MQTT_VARINT_MAX {
        return Err(());
    }

    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte)?;
        if value == 0 {
            return Ok(());
        }
    }
}

#[inline]
pub(crate) fn read_mqtt_u32_varint<E>(
    mut read: impl FnMut() -> Result<u8, E>,
    mut invalid: impl FnMut() -> E,
) -> Result<u32, E> {
    let mut value = 0u32;

    for shift in [0, 7, 14, 21] {
        let byte = read()?;
        let part = (byte & 0x7F) as u32;
        if shift == 21 && part > 0x0F {
            return Err(invalid());
        }

        value |= part << shift;
        if (byte & 0x80) == 0 {
            return Ok(value);
        }
    }

    Err(invalid())
}

impl<'de> serde::de::Visitor<'de> for VarintVisitor {
    type Value = Varint;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "Varint")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;

        let mut seq = seq;
        let value = read_mqtt_u32_varint(
            || {
                let next = seq.next_element()?;
                next.ok_or_else(|| A::Error::custom("Invalid varint"))
            },
            || A::Error::custom("Invalid varint"),
        )?;
        Ok(Varint(value))
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
        write_mqtt_u32_varint(self.0, &mut buffer)
            .map_err(|_| S::Error::custom("Failed to encode varint"))?;

        let encoded = buffer.as_slice();
        let mut seq = serializer.serialize_seq(Some(encoded.len()))?;
        for byte in encoded {
            seq.serialize_element(byte)?;
        }
        seq.end()
    }
}

#[cfg(test)]
mod tests {
    use super::{MQTT_VARINT_MAX, VarintBuffer, read_mqtt_u32_varint, write_mqtt_u32_varint};

    #[test]
    fn mqtt_varint_rejects_fourth_byte_overflow() {
        let mut bytes = [0xFF, 0xFF, 0xFF, 0xFF].into_iter();
        let result = read_mqtt_u32_varint(|| bytes.next().ok_or("missing"), || "invalid");
        assert_eq!(result, Err("invalid"));
    }

    #[test]
    fn mqtt_varint_encodes_four_bytes_max() {
        let mut buffer = VarintBuffer::new();
        write_mqtt_u32_varint(MQTT_VARINT_MAX, &mut buffer).unwrap();
        assert_eq!(buffer.as_slice(), &[0xFF, 0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn mqtt_varint_rejects_oversize_encode() {
        let mut buffer = VarintBuffer::new();
        assert_eq!(
            write_mqtt_u32_varint(MQTT_VARINT_MAX + 1, &mut buffer),
            Err(())
        );
    }
}
