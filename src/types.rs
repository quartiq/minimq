//! MQTT-specific data types used by the public API.
use crate::QoS;
use serde::Serialize;
use serde::ser::SerializeStruct;

/// MQTT binary data field.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct BinaryData<'a>(pub(crate) &'a [u8]);

impl serde::Serialize for BinaryData<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        let len = u16::try_from(self.0.len())
            .map_err(|_| S::Error::custom("Provided binary data is too long"))?;
        let mut item = serializer.serialize_struct("_BinaryData", 0)?;
        item.serialize_field("_len", &len)?;
        item.serialize_field("_data", self.0)?;
        item.end()
    }
}

struct BinaryDataVisitor;

impl<'de> serde::de::Visitor<'de> for BinaryDataVisitor {
    type Value = BinaryData<'de>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "BinaryData")
    }

    fn visit_borrowed_bytes<E: serde::de::Error>(self, data: &'de [u8]) -> Result<Self::Value, E> {
        Ok(BinaryData(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Properties, Property};
    use heapless::Vec as HVec;

    #[test]
    fn iterate_slice_properties() {
        let props = [Property::ReceiveMaximum(2), Property::MaximumQoS(1)];
        let properties = Properties::from_slice(&props);
        let values: HVec<_, 4> = properties.iter().collect();
        let expected: HVec<_, 4> =
            HVec::from_slice(&[Ok(props[0].clone()), Ok(props[1].clone())]).unwrap();
        assert_eq!(values, expected);
    }

    #[test]
    fn iterate_correlated_properties() {
        let props = [Property::ReceiveMaximum(2)];
        let correlation = Property::CorrelationData(b"abc");
        let properties = Properties::from_slice(&props).with_correlation(b"abc");
        let values: HVec<_, 4> = properties.iter().collect();
        let expected: HVec<_, 4> =
            HVec::from_slice(&[Ok(correlation), Ok(props[0].clone())]).unwrap();
        assert_eq!(values, expected);
    }

    #[test]
    fn default_subscription_options_cap_inbound_qos_to_at_most_once() {
        let filter = TopicFilter::new("demo/in");
        assert_eq!(filter.options.maximum_qos, QoS::AtMostOnce);
    }
}

impl<'de> serde::de::Deserialize<'de> for BinaryData<'de> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_bytes(BinaryDataVisitor)
    }
}

/// Username/password authentication data used in `CONNECT`.
#[derive(Debug, Copy, Clone)]
pub(crate) struct Auth<'a> {
    user_name: &'a str,
    password: &'a [u8],
}

impl<'a> Auth<'a> {
    /// Construct MQTT username/password authentication data.
    pub(crate) const fn new(user_name: &'a str, password: &'a [u8]) -> Self {
        Self {
            user_name,
            password,
        }
    }

    /// Return the MQTT username.
    pub(crate) const fn user_name(&self) -> &'a str {
        self.user_name
    }

    /// Return the MQTT password bytes.
    pub(crate) const fn password(&self) -> &'a [u8] {
        self.password
    }
}

/// MQTT UTF-8 string field.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct Utf8String<'a>(pub(crate) &'a str);

impl serde::Serialize for Utf8String<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        let len = u16::try_from(self.0.len())
            .map_err(|_| S::Error::custom("Provided string is too long"))?;
        let mut item = serializer.serialize_struct("_Utf8String", 0)?;
        item.serialize_field("_len", &len)?;
        item.serialize_field("_string", self.0)?;
        item.end()
    }
}

struct Utf8StringVisitor<'a> {
    _data: core::marker::PhantomData<&'a ()>,
}

impl<'a, 'de: 'a> serde::de::Visitor<'de> for Utf8StringVisitor<'a> {
    type Value = Utf8String<'a>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "Utf8String")
    }

    fn visit_borrowed_str<E: serde::de::Error>(self, data: &'de str) -> Result<Self::Value, E> {
        Ok(Utf8String(data))
    }
}

impl<'a, 'de: 'a> serde::de::Deserialize<'de> for Utf8String<'a> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(Utf8StringVisitor {
            _data: core::marker::PhantomData,
        })
    }
}

/// Broker retain handling policy for a subscription.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum RetainHandling {
    /// Send retained messages when the subscription is created.
    Immediately = 0b00,
    /// Send retained messages only if the subscription did not already exist.
    IfSubscriptionDoesNotExist = 0b01,
    /// Never send retained messages because of the subscription.
    Never = 0b10,
}

/// MQTT subscription options for one topic filter.
#[derive(Copy, Clone, Debug)]
pub struct SubscriptionOptions {
    maximum_qos: QoS,
    no_local: bool,
    retain_as_published: bool,
    retain_behavior: RetainHandling,
}

impl Default for SubscriptionOptions {
    fn default() -> Self {
        Self {
            maximum_qos: QoS::AtMostOnce,
            no_local: false,
            retain_as_published: false,
            retain_behavior: RetainHandling::Immediately,
        }
    }
}

impl SubscriptionOptions {
    /// Cap the maximum QoS delivered for this subscription.
    pub fn maximum_qos(mut self, qos: QoS) -> Self {
        self.maximum_qos = qos;
        self
    }

    /// Choose how retained messages are replayed when subscribing.
    pub fn retain_behavior(mut self, handling: RetainHandling) -> Self {
        self.retain_behavior = handling;
        self
    }

    /// Suppress messages published by this same client.
    pub fn ignore_local_messages(mut self) -> Self {
        self.no_local = true;
        self
    }

    /// Preserve the broker's retain flag on forwarded retained messages.
    pub fn retain_as_published(mut self) -> Self {
        self.retain_as_published = true;
        self
    }
}

impl serde::Serialize for SubscriptionOptions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = (self.maximum_qos as u8) & 0b11;
        if self.no_local {
            value |= 1 << 2;
        }
        if self.retain_as_published {
            value |= 1 << 3;
        }
        value |= (self.retain_behavior as u8) << 4;
        serializer.serialize_u8(value)
    }
}

/// Topic filter and options for `SUBSCRIBE`.
#[derive(Serialize, Copy, Clone, Debug)]
pub struct TopicFilter<'a> {
    topic: Utf8String<'a>,
    options: SubscriptionOptions,
}

impl<'a> TopicFilter<'a> {
    /// Construct a topic filter with default subscription options.
    ///
    /// ```rust
    /// use minimq::{SubscriptionOptions, TopicFilter};
    ///
    /// let filter = TopicFilter::new("demo/in").options(SubscriptionOptions::default());
    /// let _ = filter;
    /// ```
    pub fn new(topic: &'a str) -> Self {
        Self {
            topic: Utf8String(topic),
            options: SubscriptionOptions::default(),
        }
    }

    /// Override the default subscription options.
    pub fn options(mut self, options: SubscriptionOptions) -> Self {
        self.options = options;
        self
    }
}
