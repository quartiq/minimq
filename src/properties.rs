pub const PAYLOAD_FORMAT_INDICATOR: usize = 1; // Payload Format Indicator
pub const MESSAGE_EXPIRY_INTERVAL: usize = 2; // Message Expiry Interval
pub const CONTENT_TYPE: usize = 3; // Content Type
pub const RESPONSE_TOPIC: usize = 8; // Response Topic
pub const CORRELATION_DATA: usize = 9; // Correlation Data
pub const SUBSCRIPTION_IDENTIFIER: usize = 11; // Subscription Identifier
pub const SESSION_EXPIRY_INTERVAL: usize = 17; // Session Expiry Interval
pub const ASSIGNED_CLIENT_IDENTIFIER: usize = 18; // Assigned Client Identifier
pub const SERVER_KEEP_ALIVE: usize = 19; // Server Keep Alive
pub const AUTHENTICATION_METHOD: usize = 21; // Authentication Method
pub const AUTHENTICATION_DATA: usize = 22; // Authentication Data
pub const REQUEST_PROBLEM_INFORMATION: usize = 23; // Request Problem Information
pub const WILL_DELAY_INTERVAL: usize = 24; // Will Delay Interval
pub const REQUEST_RESPONSE_INFORMATION: usize = 25; // Request Response Information
pub const RESPONSE_INFORMATION: usize = 26; // Response Information
pub const SERVER_REFERENCE: usize = 28; // Server Reference
pub const REASON_STRING: usize = 31; // Reason String
pub const RECEIVE_MAXIMUM: usize = 33; // Receive Maximum
pub const TOPIC_ALIAS_MAXIMUM: usize = 34; // Topic Alias Maximum
pub const TOPIC_ALIAS: usize = 35; // Topic Alias
pub const MAXIMUM_QOS: usize = 36; // Maximum QoS
pub const RETAIN_AVAILABLE: usize = 37; // Retain Available
pub const USER_PROPERTY: usize = 38; // User Property
pub const MAXIMUM_PACKET_SIZE: usize = 39; // Maximum Packet Size
pub const WILDCARD_SUBSCRIPTION_AVAILABLE: usize = 40; // Wildcard Subscription Available
pub const SUBSCRIPTION_IDENTIFIER_AVAILABLE: usize = 41; // Subscription Identifier Available
pub const SHARED_SUBSCRIPTION_AVAILABLE: usize = 42; // Shared Subscription Available

pub enum Property<'a> {
    ResponseTopic(&'a str),
}

const PROPERTY_MAX: usize = 43;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Data {
    Byte,
    TwoByteInteger,
    FourByteInteger,
    VariableByteInteger,
    BinaryData,
    UTF8EncodedString,
    UTF8StringPair,
}

const PROPERTY_DATA: [Option<Data>; PROPERTY_MAX] = [
    None,
    Some(Data::Byte),              // Payload Format Indicator
    Some(Data::FourByteInteger),   // Message Expiry Interval
    Some(Data::UTF8EncodedString), // Content Type
    None,
    None,
    None,
    None,
    Some(Data::UTF8EncodedString), // Response Topic
    Some(Data::BinaryData),        // Correlation Data
    None,
    Some(Data::VariableByteInteger), // Subscription Identifier
    None,
    None,
    None,
    None,
    None,
    Some(Data::FourByteInteger),   // Session Expiry Interval
    Some(Data::UTF8EncodedString), // Assigned Client Identifier
    Some(Data::TwoByteInteger),    // Server Keep Alive
    None,
    Some(Data::UTF8EncodedString), // Authentication Method
    Some(Data::BinaryData),        // Authentication Data
    Some(Data::Byte),              // Request Problem Information
    Some(Data::FourByteInteger),   // Will Delay Interval
    Some(Data::Byte),              // Request Response Information
    Some(Data::UTF8EncodedString), // Response Information
    None,
    Some(Data::UTF8EncodedString), // Server Reference
    None,
    None,
    Some(Data::UTF8EncodedString), // Reason String
    None,
    Some(Data::TwoByteInteger),  // Receive Maximum
    Some(Data::TwoByteInteger),  // Topic Alias Maximum
    Some(Data::TwoByteInteger),  // Topic Alias
    Some(Data::Byte),            // Maximum QoS
    Some(Data::Byte),            // Retain Available
    Some(Data::UTF8StringPair),  // User Property
    Some(Data::FourByteInteger), // Maximum Packet Size
    Some(Data::Byte),            // Wildcard Subscription Available
    Some(Data::Byte),            // Subscription Identifier Available
    Some(Data::Byte),            // Shared Subscription Available
];

pub fn property_data(property: usize) -> Option<Data> {
    if property < PROPERTY_DATA.len() {
        PROPERTY_DATA[property]
    } else {
        None
    }
}
