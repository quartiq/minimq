use serde::ser::SerializeSeq;

const MQTT_VARINT_DATA_BITS: usize = 7;
const MQTT_VARINT_MAX_BYTES: usize = 4;
const MQTT_VARINT_DATA_MASK: u8 = (1 << MQTT_VARINT_DATA_BITS) - 1;
const MQTT_VARINT_CONTINUATION: u8 = 1 << MQTT_VARINT_DATA_BITS;
const MQTT_VARINT_BASE: u32 = 1 << MQTT_VARINT_DATA_BITS;
pub(crate) const MQTT_VARINT_MAX: u32 = (1 << (MQTT_VARINT_DATA_BITS * MQTT_VARINT_MAX_BYTES)) - 1;

#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct Varint(pub(crate) u32);

impl Varint {
    /// Return the encoded length for a valid MQTT variable byte integer.
    pub(crate) fn encoded_len(&self) -> usize {
        let mut value = self.0;
        let mut len = 1;
        while value >= MQTT_VARINT_BASE {
            value /= MQTT_VARINT_BASE;
            len += 1;
        }
        len
    }
}

impl From<u32> for Varint {
    fn from(val: u32) -> Varint {
        Varint(val)
    }
}

pub(crate) struct VarintBuffer {
    data: [u8; MQTT_VARINT_MAX_BYTES],
    len: u8,
}

impl VarintBuffer {
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            data: [0; MQTT_VARINT_MAX_BYTES],
            len: 0,
        }
    }

    #[inline]
    pub(crate) fn as_slice(&self) -> &[u8] {
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

/// Encode one MQTT variable byte integer into a fixed four-byte scratch buffer.
#[inline]
pub(crate) fn write_mqtt_u32_varint(mut value: u32, out: &mut VarintBuffer) -> Result<(), ()> {
    if value > MQTT_VARINT_MAX {
        return Err(());
    }

    loop {
        let mut byte = (value & u32::from(MQTT_VARINT_DATA_MASK)) as u8;
        value >>= MQTT_VARINT_DATA_BITS;
        if value != 0 {
            byte |= MQTT_VARINT_CONTINUATION;
        }
        out.push(byte)?;
        if value == 0 {
            return Ok(());
        }
    }
}

/// Decode one canonical MQTT variable byte integer.
///
/// Rejects overlong encodings, values above the MQTT 28-bit maximum, and
/// sequences that do not terminate within four bytes.
#[inline]
pub(crate) fn read_mqtt_u32_varint<E>(
    mut read: impl FnMut() -> Result<u8, E>,
    mut invalid: impl FnMut() -> E,
) -> Result<u32, E> {
    let mut value = 0u32;

    for index in 0..MQTT_VARINT_MAX_BYTES {
        let shift = index * MQTT_VARINT_DATA_BITS;
        let byte = read()?;
        let part = u32::from(byte & MQTT_VARINT_DATA_MASK);
        value |= part << shift;
        if value > MQTT_VARINT_MAX {
            return Err(invalid());
        }

        if (byte & MQTT_VARINT_CONTINUATION) == 0 {
            if shift != 0 && part == 0 {
                return Err(invalid());
            }
            return Ok(value);
        }
    }

    Err(invalid())
}

#[derive(Copy, Clone)]
enum ProbeError {
    Incomplete,
    Invalid,
}

/// Probe a possibly incomplete MQTT variable byte integer.
pub(crate) fn probe_mqtt_u32_varint(bytes: &[u8]) -> Result<Option<(u32, usize)>, ()> {
    let mut len = 0;
    match read_mqtt_u32_varint(
        || {
            let byte = bytes.get(len).copied().ok_or(ProbeError::Incomplete)?;
            len += 1;
            Ok(byte)
        },
        || ProbeError::Invalid,
    ) {
        Ok(value) => Ok(Some((value, len))),
        Err(ProbeError::Incomplete) => Ok(None),
        Err(ProbeError::Invalid) => Err(()),
    }
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
        deserializer.deserialize_tuple(MQTT_VARINT_MAX_BYTES, VarintVisitor)
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
    fn mqtt_varint_rejects_overlong_zero() {
        let mut bytes = [0x80, 0x00].into_iter();
        let result = read_mqtt_u32_varint(|| bytes.next().ok_or("missing"), || "invalid");
        assert_eq!(result, Err("invalid"));
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
