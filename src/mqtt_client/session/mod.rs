mod drive;
mod handshake;
mod inbound;
mod operations;
mod state;

#[cfg(test)]
mod tests;

use crate::de::PacketReader;
use crate::ser::MAX_FIXED_HEADER_SIZE;
use crate::types::Auth;
use crate::{ConfigBuilder, Op, QoS, Will};
use heapless::String;

use super::{ConnectEvent, Io, OpKind, OpStatus};

use state::{RuntimeState, SessionData};

/// One long-lived MQTT client session.
///
/// Drive the session after [`connect`](Self::connect) has taken ownership of a live transport.
/// Use [`recv`](Self::recv) when you want the next inbound publish, [`poll`](Self::poll) when you
/// need to wait for any session progress, and [`drive`](Self::drive) for cooperative immediate
/// progress. Real time bounds come from the transport: stalled reads, writes, or flushes must
/// eventually error if the caller needs hard latency limits. The same session is also used for
/// outbound `publish`, `subscribe`, and `unsubscribe` operations.
///
/// Cancel safety, assuming the transport's I/O futures are cancel-safe:
/// [`drive`](Self::drive), [`poll`](Self::poll), [`recv`](Self::recv), [`disconnect`](Self::disconnect),
/// [`subscribe`](Self::subscribe), [`unsubscribe`](Self::unsubscribe), and
/// [`publish`](Self::publish) for QoS 1/2 preserve local session state across cancellation.
/// Cancelling [`connect`](Self::connect) drops the supplied transport and leaves the session
/// disconnected; the next `connect()` retries from clean transport-local state. QoS 0
/// [`publish`](Self::publish) is not cancel-safe because it bypasses retained outbound state and
/// writes directly from temporary TX scratch space.
pub struct Session<'buf> {
    client_id: String<64>,
    packet_reader: PacketReader<'buf>,
    data: SessionData<'buf>,
    runtime: RuntimeState,
    will: Option<Will<'buf>>,
    auth: Option<Auth<'buf>>,
    session_expiry_interval: u32,
    downgrade_qos: bool,
}

impl<'buf> Session<'buf> {
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

    /// Return the maximum inbound MQTT packet size accepted by this session.
    pub fn max_rx_packet_size(&self) -> usize {
        self.packet_reader.capacity()
    }

    /// Return the maximum outbound MQTT packet arena size available to this session.
    pub fn max_tx_packet_size(&self) -> usize {
        self.data.outbound.capacity()
    }

    /// Return whether the session currently has the local capacity to attempt a
    /// publish at the requested QoS.
    fn can_publish(&self, qos: QoS) -> bool {
        if qos == QoS::AtMostOnce {
            self.data.outbound.scratch_len() >= MAX_FIXED_HEADER_SIZE
        } else {
            self.runtime.send_quota != 0 && self.data.outbound.can_retain()
        }
    }

    /// Return whether the session has no in-flight retained MQTT packets or
    /// pending release state.
    pub fn is_publish_quiescent(&self) -> bool {
        self.data.outbound.is_quiescent()
    }

    fn status(&self, op: &Op) -> OpStatus {
        if op.generation != self.data.generation() {
            return OpStatus::Invalidated;
        }

        let pending = match op.kind {
            OpKind::PublishAtLeastOnce | OpKind::Subscribe | OpKind::Unsubscribe => {
                self.data.outbound.has_retained(op.packet_id)
            }
            OpKind::PublishExactlyOnce => {
                self.data.outbound.has_retained(op.packet_id)
                    || self.data.outbound.has_pending_release(op.packet_id)
            }
        };

        if pending {
            OpStatus::Pending
        } else {
            OpStatus::Complete
        }
    }

    /// Return whether the operation still has local in-flight state.
    pub fn is_pending(&self, op: &Op) -> bool {
        self.status(op) == OpStatus::Pending
    }

    /// Return whether the operation completed in the current session generation.
    pub fn is_complete(&self, op: &Op) -> bool {
        self.status(op) == OpStatus::Complete
    }

    /// Return whether the local session state that could complete this operation was discarded.
    pub fn is_invalidated(&self, op: &Op) -> bool {
        self.status(op) == OpStatus::Invalidated
    }
}

/// A live MQTT connection over a transport `IO`, returned by
/// [`Session::connect`](Session::connect).
///
/// The handle owns the transport and borrows the [`Session`] for the duration of the connection.
/// All network operations (`drive`, `poll`, `recv`, `publish`, `subscribe`, `unsubscribe`,
/// `disconnect`) live here; session-state queries (`is_pending`, `is_complete`, `can_publish`, …)
/// are reachable through the [`Self::session`] method.
///
/// Note that dropping or forgetting the handle is an *ungraceful* MQTT close: **no `DISCONNECT`
/// packet is sent** (a sync `Drop` cannot perform the async write).
pub struct Connection<'a, 'buf, IO> {
    pub(super) session: &'a mut Session<'buf>,
    pub(super) io: IO,
    pub(super) event: ConnectEvent,
    pub(super) live: bool,
}

impl<'buf, IO: Io> Connection<'_, 'buf, IO> {
    /// Whether this connection started a fresh broker session or resumed an existing one.
    pub fn connect_event(&self) -> ConnectEvent {
        self.event
    }

    /// Return a reference to the underlying session.
    pub fn session(&self) -> &Session<'buf> {
        self.session
    }

    /// Return whether the transport is connected and the session currently has the local capacity to attempt a
    /// publish at the requested QoS.
    pub fn can_publish(&self, qos: QoS) -> bool {
        self.live && self.session.can_publish(qos)
    }

    /// Whether the connection is still live.
    ///
    /// Becomes `false` once any operation on this handle has observed a transport- or
    /// protocol-level disconnect (broker `DISCONNECT`, transport error, keepalive timeout, fatal
    /// protocol violation) or after a graceful [`disconnect`](Self::disconnect). Once `false`,
    /// further operations fail fast with [`Error::Disconnected`](crate::Error::Disconnected)
    /// instead of driving the dead transport; drop the handle and `connect` again to recover.
    pub fn is_connected(&self) -> bool {
        self.live
    }

    /// Return whether the operation still has local in-flight state.
    pub fn is_pending(&self, op: &Op) -> bool {
        self.session.is_pending(op)
    }

    /// Return whether the operation completed in the current session generation.
    pub fn is_complete(&self, op: &Op) -> bool {
        self.session.is_complete(op)
    }

    /// Return whether the local session state that could complete this operation was discarded.
    pub fn is_invalidated(&self, op: &Op) -> bool {
        self.session.is_invalidated(op)
    }

    /// Return the underlying transport, consuming this handle.
    pub fn into_inner(mut self) -> IO {
        self.handle_disconnect();
        self.io
    }

    pub fn handle_disconnect(&mut self) {
        self.live = false;
        self.session.handle_disconnect();
    }
}
