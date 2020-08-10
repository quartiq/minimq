use crate::{
    de::PacketReader,
    mqtt_client::ProtocolError as Error,
    ser::{serialize::integer_size, ReversedPacketWriter},
};

use enum_iterator::IntoEnumIterator;
use generic_array::ArrayLength;

#[derive(Copy, Clone, IntoEnumIterator)]
pub(crate) enum PropertyIdentifier {
    Invalid = -1,

    PayloadFormatIndicator = 0x01,
    MessageExpiryInterval = 0x02,
    ContentType = 0x03,

    ResponseTopic = 0x08,
    CorrelationData = 0x09,

    SubscriptionIdentifier = 0x0B,

    SessionExpiryInterval = 0x11,
    AssignedClientIdentifier = 0x12,
    ServerKeepAlive = 0x13,
    AuthenticationMethod = 0x15,
    AuthenticationData = 0x16,
    RequestProblemInformation = 0x17,
    WillDelayInterval = 0x18,
    RequestResponseInformation = 0x19,

    ResponseInformation = 0x1A,

    ServerReference = 0x1C,

    ReasonString = 0x1F,

    ReceiveMaximum = 0x21,
    TopicAliasMaximum = 0x22,
    TopicAlias = 0x23,
    MaximumQoS = 0x24,
    RetainAvailable = 0x25,
    UserProperty = 0x26,
    MaximumPacketSize = 0x27,
    WildcardSubscriptionAvailable = 0x28,
    SubscriptionIdentifierAvailable = 0x29,
    SharedSubscriptionAvailable = 0x2A,
}

/// All of the possible properties that MQTT version 5 supports.
#[derive(Debug)]
pub enum Property<'a> {
    PayloadFormatIndicator(u8),
    MessageExpiryInterval(u32),
    ContentType(&'a str),
    ResponseTopic(&'a str),
    CorrelationData(&'a [u8]),
    SubscriptionIdentifier(usize),
    SessionExpiryInterval(u32),
    AssignedClientIdentifier(&'a str),
    ServerKeepAlive(u16),
    AuthenticationMethod(&'a str),
    AuthenticationData(&'a [u8]),
    RequestProblemInformation(u8),
    WillDelayInterval(u32),
    RequestResponseInformation(u8),
    ResponseInformation(&'a str),
    ServerReference(&'a str),
    ReasonString(&'a str),
    ReceiveMaximum(u16),
    TopicAliasMaximum(u16),
    TopicAlias(u16),
    MaximumQoS(u8),
    RetainAvailable(u8),
    UserProperty(&'a str, &'a str),
    MaximumPacketSize(u32),
    WildcardSubscriptionAvailable(u8),
    SubscriptionIdentifierAvailable(u8),
    SharedSubscriptionAvailable(u8),
}

impl From<usize> for PropertyIdentifier {
    fn from(val: usize) -> Self {
        for entry in Self::into_enum_iter() {
            if entry as usize == val {
                return entry;
            }
        }

        return PropertyIdentifier::Invalid;
    }
}

impl<'a> Property<'a> {
    pub(crate) fn id(&self) -> PropertyIdentifier {
        match self {
            Property::PayloadFormatIndicator(_) => PropertyIdentifier::PayloadFormatIndicator,
            Property::MessageExpiryInterval(_) => PropertyIdentifier::MessageExpiryInterval,
            Property::ContentType(_) => PropertyIdentifier::ContentType,
            Property::ResponseTopic(_) => PropertyIdentifier::ResponseTopic,
            Property::CorrelationData(_) => PropertyIdentifier::CorrelationData,
            Property::SubscriptionIdentifier(_) => PropertyIdentifier::SubscriptionIdentifier,
            Property::SessionExpiryInterval(_) => PropertyIdentifier::SessionExpiryInterval,
            Property::AssignedClientIdentifier(_) => PropertyIdentifier::AssignedClientIdentifier,
            Property::ServerKeepAlive(_) => PropertyIdentifier::ServerKeepAlive,
            Property::AuthenticationMethod(_) => PropertyIdentifier::AuthenticationMethod,
            Property::AuthenticationData(_) => PropertyIdentifier::AuthenticationData,
            Property::RequestProblemInformation(_) => PropertyIdentifier::RequestProblemInformation,
            Property::WillDelayInterval(_) => PropertyIdentifier::WillDelayInterval,
            Property::RequestResponseInformation(_) => {
                PropertyIdentifier::RequestResponseInformation
            }
            Property::ResponseInformation(_) => PropertyIdentifier::ResponseInformation,
            Property::ServerReference(_) => PropertyIdentifier::ServerReference,
            Property::ReasonString(_) => PropertyIdentifier::ReasonString,
            Property::ReceiveMaximum(_) => PropertyIdentifier::ReceiveMaximum,
            Property::TopicAliasMaximum(_) => PropertyIdentifier::TopicAliasMaximum,
            Property::TopicAlias(_) => PropertyIdentifier::TopicAlias,
            Property::MaximumQoS(_) => PropertyIdentifier::MaximumQoS,
            Property::RetainAvailable(_) => PropertyIdentifier::RetainAvailable,
            Property::UserProperty(_, _) => PropertyIdentifier::UserProperty,
            Property::MaximumPacketSize(_) => PropertyIdentifier::MaximumPacketSize,
            Property::WildcardSubscriptionAvailable(_) => {
                PropertyIdentifier::WildcardSubscriptionAvailable
            }
            Property::SubscriptionIdentifierAvailable(_) => {
                PropertyIdentifier::SubscriptionIdentifierAvailable
            }
            Property::SharedSubscriptionAvailable(_) => {
                PropertyIdentifier::SharedSubscriptionAvailable
            }
        }
    }

    pub(crate) fn parse<'reader: 'a, T: ArrayLength<u8>>(
        packet: &'reader PacketReader<T>,
    ) -> Result<Property<'a>, Error> {
        let identifier: PropertyIdentifier = packet.read_variable_length_integer()?.into();

        match identifier {
            PropertyIdentifier::ResponseTopic => {
                Ok(Property::ResponseTopic(packet.read_utf8_string()?))
            }
            PropertyIdentifier::PayloadFormatIndicator => {
                Ok(Property::PayloadFormatIndicator(packet.read_u8()?))
            }
            PropertyIdentifier::MessageExpiryInterval => {
                Ok(Property::MessageExpiryInterval(packet.read_u32()?))
            }
            PropertyIdentifier::ContentType => {
                Ok(Property::ContentType(packet.read_utf8_string()?))
            }
            PropertyIdentifier::CorrelationData => {
                Ok(Property::CorrelationData(packet.read_binary_data()?))
            }
            PropertyIdentifier::SubscriptionIdentifier => Ok(Property::SubscriptionIdentifier(
                packet.read_variable_length_integer()?,
            )),
            PropertyIdentifier::SessionExpiryInterval => {
                Ok(Property::SessionExpiryInterval(packet.read_u32()?))
            }
            PropertyIdentifier::AssignedClientIdentifier => Ok(Property::AssignedClientIdentifier(
                packet.read_utf8_string()?,
            )),
            PropertyIdentifier::ServerKeepAlive => {
                Ok(Property::ServerKeepAlive(packet.read_u16()?))
            }
            PropertyIdentifier::AuthenticationMethod => {
                Ok(Property::AuthenticationMethod(packet.read_utf8_string()?))
            }
            PropertyIdentifier::AuthenticationData => {
                Ok(Property::AuthenticationData(packet.read_binary_data()?))
            }
            PropertyIdentifier::RequestProblemInformation => {
                Ok(Property::RequestProblemInformation(packet.read_u8()?))
            }
            PropertyIdentifier::WillDelayInterval => {
                Ok(Property::WillDelayInterval(packet.read_u32()?))
            }
            PropertyIdentifier::RequestResponseInformation => {
                Ok(Property::RequestResponseInformation(packet.read_u8()?))
            }
            PropertyIdentifier::ResponseInformation => {
                Ok(Property::ResponseInformation(packet.read_utf8_string()?))
            }
            PropertyIdentifier::ServerReference => {
                Ok(Property::ServerReference(packet.read_utf8_string()?))
            }
            PropertyIdentifier::ReasonString => {
                Ok(Property::ReasonString(packet.read_utf8_string()?))
            }
            PropertyIdentifier::ReceiveMaximum => Ok(Property::ReceiveMaximum(packet.read_u16()?)),
            PropertyIdentifier::TopicAliasMaximum => {
                Ok(Property::TopicAliasMaximum(packet.read_u16()?))
            }
            PropertyIdentifier::TopicAlias => Ok(Property::TopicAlias(packet.read_u16()?)),
            PropertyIdentifier::MaximumQoS => Ok(Property::MaximumQoS(packet.read_u8()?)),
            PropertyIdentifier::RetainAvailable => Ok(Property::RetainAvailable(packet.read_u8()?)),
            PropertyIdentifier::UserProperty => Ok(Property::UserProperty(
                packet.read_utf8_string()?,
                packet.read_utf8_string()?,
            )),
            PropertyIdentifier::MaximumPacketSize => {
                Ok(Property::MaximumPacketSize(packet.read_u32()?))
            }
            PropertyIdentifier::WildcardSubscriptionAvailable => {
                Ok(Property::WildcardSubscriptionAvailable(packet.read_u8()?))
            }
            PropertyIdentifier::SubscriptionIdentifierAvailable => {
                Ok(Property::SubscriptionIdentifierAvailable(packet.read_u8()?))
            }
            PropertyIdentifier::SharedSubscriptionAvailable => {
                Ok(Property::SharedSubscriptionAvailable(packet.read_u8()?))
            }

            _ => Err(Error::Invalid),
        }
    }

    pub(crate) fn size(&self) -> usize {
        // Although property identifiers are technically encoded as variable length integers, in
        // practice, they are all small enough to fit in 1 byte.
        let identifier_length = 1;

        match self {
            Property::ContentType(data)
            | Property::ResponseTopic(data)
            | Property::AuthenticationMethod(data)
            | Property::ResponseInformation(data)
            | Property::ServerReference(data)
            | Property::ReasonString(data)
            | Property::AssignedClientIdentifier(data) => data.len() + 2 + identifier_length,
            Property::UserProperty(key, value) => {
                (value.len() + 2) + (key.len() + 2) + identifier_length
            }
            Property::CorrelationData(data) | Property::AuthenticationData(data) => {
                data.len() + 2 + identifier_length
            }
            Property::SubscriptionIdentifier(id) => integer_size(*id),

            Property::MessageExpiryInterval(_)
            | Property::SessionExpiryInterval(_)
            | Property::WillDelayInterval(_)
            | Property::MaximumPacketSize(_) => 4 + identifier_length,
            Property::ServerKeepAlive(_)
            | Property::ReceiveMaximum(_)
            | Property::TopicAliasMaximum(_)
            | Property::TopicAlias(_) => 2 + identifier_length,
            Property::PayloadFormatIndicator(_)
            | Property::RequestProblemInformation(_)
            | Property::RequestResponseInformation(_)
            | Property::MaximumQoS(_)
            | Property::RetainAvailable(_)
            | Property::WildcardSubscriptionAvailable(_)
            | Property::SubscriptionIdentifierAvailable(_)
            | Property::SharedSubscriptionAvailable(_) => 1 + identifier_length,
        }
    }

    pub(crate) fn encode_into<'b>(
        &self,
        packet: &mut ReversedPacketWriter<'b>,
    ) -> Result<(), Error> {
        match self {
            Property::PayloadFormatIndicator(value) => Property::encode_byte_property(
                packet,
                PropertyIdentifier::PayloadFormatIndicator,
                value,
            ),
            Property::MessageExpiryInterval(value) => Property::encode_four_byte_integer_property(
                packet,
                PropertyIdentifier::MessageExpiryInterval,
                value,
            ),
            Property::ContentType(content_type) => Property::encode_string_property(
                packet,
                PropertyIdentifier::ContentType,
                content_type,
            ),
            Property::ResponseTopic(topic) => {
                Property::encode_string_property(packet, PropertyIdentifier::ResponseTopic, topic)
            }
            Property::CorrelationData(data) => Property::encode_binary_data_property(
                packet,
                PropertyIdentifier::CorrelationData,
                data,
            ),
            Property::SubscriptionIdentifier(data) => {
                Property::encode_variable_length_integer_property(
                    packet,
                    PropertyIdentifier::SubscriptionIdentifier,
                    data,
                )
            }
            Property::SessionExpiryInterval(data) => Property::encode_four_byte_integer_property(
                packet,
                PropertyIdentifier::SessionExpiryInterval,
                data,
            ),
            Property::AssignedClientIdentifier(data) => Property::encode_string_property(
                packet,
                PropertyIdentifier::AssignedClientIdentifier,
                data,
            ),
            Property::ServerKeepAlive(data) => Property::encode_two_byte_integer_property(
                packet,
                PropertyIdentifier::ServerKeepAlive,
                data,
            ),
            Property::AuthenticationMethod(data) => Property::encode_string_property(
                packet,
                PropertyIdentifier::AuthenticationMethod,
                data,
            ),
            Property::AuthenticationData(data) => Property::encode_binary_data_property(
                packet,
                PropertyIdentifier::AuthenticationData,
                data,
            ),
            Property::RequestProblemInformation(data) => Property::encode_byte_property(
                packet,
                PropertyIdentifier::RequestProblemInformation,
                data,
            ),
            Property::WillDelayInterval(data) => Property::encode_four_byte_integer_property(
                packet,
                PropertyIdentifier::WillDelayInterval,
                data,
            ),
            Property::RequestResponseInformation(data) => Property::encode_byte_property(
                packet,
                PropertyIdentifier::RequestResponseInformation,
                data,
            ),
            Property::ResponseInformation(data) => Property::encode_string_property(
                packet,
                PropertyIdentifier::ResponseInformation,
                data,
            ),
            Property::ServerReference(data) => {
                Property::encode_string_property(packet, PropertyIdentifier::ServerReference, data)
            }
            Property::ReasonString(data) => {
                Property::encode_string_property(packet, PropertyIdentifier::ReasonString, data)
            }
            Property::ReceiveMaximum(data) => Property::encode_two_byte_integer_property(
                packet,
                PropertyIdentifier::ReceiveMaximum,
                data,
            ),
            Property::TopicAliasMaximum(data) => Property::encode_two_byte_integer_property(
                packet,
                PropertyIdentifier::TopicAliasMaximum,
                data,
            ),
            Property::TopicAlias(data) => Property::encode_two_byte_integer_property(
                packet,
                PropertyIdentifier::TopicAlias,
                data,
            ),
            Property::MaximumQoS(data) => {
                Property::encode_byte_property(packet, PropertyIdentifier::MaximumQoS, data)
            }
            Property::RetainAvailable(data) => {
                Property::encode_byte_property(packet, PropertyIdentifier::RetainAvailable, data)
            }
            Property::UserProperty(key, value) => Property::encode_string_pair_property(
                packet,
                PropertyIdentifier::UserProperty,
                key,
                value,
            ),
            Property::MaximumPacketSize(data) => Property::encode_four_byte_integer_property(
                packet,
                PropertyIdentifier::MaximumPacketSize,
                data,
            ),
            Property::WildcardSubscriptionAvailable(data) => Property::encode_byte_property(
                packet,
                PropertyIdentifier::WildcardSubscriptionAvailable,
                data,
            ),
            Property::SubscriptionIdentifierAvailable(data) => Property::encode_byte_property(
                packet,
                PropertyIdentifier::SubscriptionIdentifierAvailable,
                data,
            ),
            Property::SharedSubscriptionAvailable(data) => Property::encode_byte_property(
                packet,
                PropertyIdentifier::SharedSubscriptionAvailable,
                data,
            ),
        }
    }

    fn encode_string_property<'b>(
        packet: &mut ReversedPacketWriter<'b>,
        id: PropertyIdentifier,
        text: &'a str,
    ) -> Result<(), Error> {
        packet.write_utf8_string(text)?;
        packet.write_variable_length_integer(id as usize)?;

        Ok(())
    }

    fn encode_byte_property<'b>(
        packet: &mut ReversedPacketWriter<'b>,
        id: PropertyIdentifier,
        byte: &u8,
    ) -> Result<(), Error> {
        packet.write(&[*byte])?;
        packet.write_variable_length_integer(id as usize)?;

        Ok(())
    }

    fn encode_variable_length_integer_property<'b>(
        packet: &mut ReversedPacketWriter<'b>,
        id: PropertyIdentifier,
        value: &usize,
    ) -> Result<(), Error> {
        packet.write_variable_length_integer(*value)?;
        packet.write_variable_length_integer(id as usize)?;

        Ok(())
    }

    fn encode_four_byte_integer_property<'b>(
        packet: &mut ReversedPacketWriter<'b>,
        id: PropertyIdentifier,
        value: &u32,
    ) -> Result<(), Error> {
        packet.write_u32(*value)?;
        packet.write_variable_length_integer(id as usize)?;

        Ok(())
    }

    fn encode_binary_data_property<'b>(
        packet: &mut ReversedPacketWriter<'b>,
        id: PropertyIdentifier,
        data: &[u8],
    ) -> Result<(), Error> {
        packet.write_binary_data(data)?;
        packet.write_variable_length_integer(id as usize)?;

        Ok(())
    }

    fn encode_two_byte_integer_property<'b>(
        packet: &mut ReversedPacketWriter<'b>,
        id: PropertyIdentifier,
        value: &u16,
    ) -> Result<(), Error> {
        packet.write_u16(*value)?;
        packet.write_variable_length_integer(id as usize)?;

        Ok(())
    }

    fn encode_string_pair_property<'b>(
        packet: &mut ReversedPacketWriter<'b>,
        id: PropertyIdentifier,
        key: &'a str,
        value: &'a str,
    ) -> Result<(), Error> {
        packet.write_utf8_string(value)?;
        packet.write_utf8_string(key)?;
        packet.write_variable_length_integer(id as usize)?;

        Ok(())
    }
}
