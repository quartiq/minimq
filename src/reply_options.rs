use crate::{properties::Property, types::BinaryData};

/// Used to specify information about replies to received MQTT messages.
pub struct ReplyOptions<'a> {
    pub(crate) response_topic: Option<&'a str>,
    pub(crate) correlation_data: Option<BinaryData<'a>>,
    pub(crate) ignore_missing_response: bool,
}

impl<'a> ReplyOptions<'a> {
    /// Construct options for a reply from in-bound message properties.
    ///
    /// # Note
    /// This API extracts the `ResponseTopic` from received properties to publish the reply to.
    ///
    /// `CorrelationData` present in the received properties will also be copied over.
    ///
    /// # Args
    /// * `properties` - The properties of the message being replied to.
    pub fn new(properties: &[Property<'a>]) -> Self {
        // First, extract the response topic from the inbound properties.
        let response_topic = properties.iter().find_map(|p| {
            if let Property::ResponseTopic(topic) = p {
                Some(topic.0)
            } else {
                None
            }
        });

        // Next, copy over any correlation data to the outbound properties.
        let correlation_data = properties.iter().find_map(|p| {
            if let Property::CorrelationData(data) = p {
                Some(*data)
            } else {
                None
            }
        });

        Self {
            response_topic,
            correlation_data,
            ignore_missing_response: false,
        }
    }

    /// If specified, an outbound message without an identified response topic will be silently
    /// dropped.
    pub fn ignore_missing_response_topic(mut self) -> Self {
        self.ignore_missing_response = true;
        self
    }

    /// Specify a default response topic if no `Property::ResponseTopic` is found.
    ///
    /// # Args
    /// * `topic` - The default response topic to use.
    pub fn default_response_topic(mut self, topic: &'a str) -> Self {
        self.response_topic = self.response_topic.or(Some(topic));
        self
    }
}
