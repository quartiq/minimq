//! MQTT-specific data types used by the public API.
use crate::{QoS, wire::Utf8String};
use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use super::TopicFilter;
    use crate::QoS;

    #[test]
    fn default_subscription_options_cap_inbound_qos_to_at_most_once() {
        let filter = TopicFilter::new("demo/in");
        assert_eq!(filter.options.maximum_qos, QoS::AtMostOnce);
    }
}
