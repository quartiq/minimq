mod drive;
mod handshake;
mod inbound;
mod operations;
mod state;

#[cfg(test)]
mod tests;

pub(super) use super::outbound::{
    ControlAction, Outbound, OutboundStep, SendState, check_control_packet_size, check_pubrel_size,
    serialize_control_packet, serialize_pubrel, write_packet,
};
pub(super) use super::{ConnectEvent, Event, InboundPublish, Io};
pub(super) use crate::de::PacketReader;
pub(super) use crate::de::received_packet::ReceivedPacket;
pub(super) use crate::packets::{Connect, DisconnectReq, Pub, Subscribe, Unsubscribe};
pub(super) use crate::ser::MAX_FIXED_HEADER_SIZE;
pub(super) use crate::types::{Auth, Properties, TopicFilter, Utf8String};
pub(super) use crate::{
    ConfigBuilder, Error, Property, ProtocolError, PubError, QoS, ReasonCode, Will, debug, info,
    trace, warn,
};
pub(super) use core::convert::{Infallible, TryFrom};
pub(super) use core::num::NonZeroU16;
pub(super) use embassy_time::{Duration, Instant, with_deadline};
pub(super) use heapless::{String, Vec};

use state::{RuntimeState, SessionData};

/// One long-lived MQTT client session.
///
/// Drive the session by calling [`poll`](Self::poll) regularly after
/// [`connect`](Self::connect) has taken ownership of a live transport. The same session is also
/// used for outbound `publish`, `subscribe`, and `unsubscribe` operations.
///
/// Cancel safety, assuming the transport's I/O futures are cancel-safe:
/// [`poll`](Self::poll), [`disconnect`](Self::disconnect), [`subscribe`](Self::subscribe),
/// [`unsubscribe`](Self::unsubscribe), and [`publish`](Self::publish) for QoS 1/2 preserve local
/// session state across cancellation. Cancelling [`connect`](Self::connect) drops the supplied
/// transport and leaves the session disconnected; the next `connect()` retries from clean
/// transport-local state. QoS 0 [`publish`](Self::publish) is not cancel-safe because it bypasses
/// retained outbound state and writes directly from temporary TX scratch space.
pub struct Session<'buf, IO> {
    connection: Option<IO>,
    client_id: String<64>,
    packet_reader: PacketReader<'buf>,
    data: SessionData<'buf>,
    runtime: RuntimeState,
    will: Option<Will<'buf>>,
    auth: Option<Auth<'buf>>,
    session_expiry_interval: u32,
    downgrade_qos: bool,
}

impl<'buf, IO> Session<'buf, IO>
where
    IO: Io,
{
    /// Construct a session from a setup builder.
    pub fn new(config: ConfigBuilder<'buf>) -> Self {
        let (
            buffers,
            will,
            client_id,
            keepalive_interval,
            session_expiry_interval,
            downgrade_qos,
            auth,
        ) = config.into_parts();
        let (rx, tx) = buffers.into_parts();

        Self {
            connection: None,
            client_id,
            packet_reader: PacketReader::new(rx),
            data: SessionData::new(tx),
            runtime: RuntimeState::new(keepalive_interval),
            will,
            auth,
            session_expiry_interval,
            downgrade_qos,
        }
    }

    /// Return whether the MQTT session is currently established.
    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Return whether the session currently has the local capacity to attempt a
    /// publish at the requested QoS.
    pub fn can_publish(&mut self, qos: QoS) -> bool {
        self.connection.is_some()
            && if qos == QoS::AtMostOnce {
                self.data.outbound.scratch_space().len() >= MAX_FIXED_HEADER_SIZE
            } else {
                self.runtime.send_quota != 0 && self.data.outbound.can_retain()
            }
    }

    /// Return whether the session has no in-flight retained MQTT packets or
    /// pending release state.
    pub fn is_publish_quiescent(&self) -> bool {
        self.data.outbound.is_quiescent()
    }
}
