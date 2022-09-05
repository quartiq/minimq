//! # Design Parameters
//! This module contains design constraints arbitrarily imposed on the library.

/// The maximum number of subscriptions supported in a single request.
pub const MAX_TOPICS_PER_SUBSCRIPTION: usize = 8;

/// The maximum number of properties that can be received in a single message.
pub const MAX_RX_PROPERTIES: usize = 8;
