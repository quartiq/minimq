mod deserializer;
pub mod varint;

pub fn deserialize<'a, T: serde::de::Deserialize<'a>>(
    buf: &'a [u8],
) -> Result<T, deserializer::Error> {
    let mut deserializer = deserializer::MqttDeserializer::new(buf);
    T::deserialize(&mut deserializer)
}
