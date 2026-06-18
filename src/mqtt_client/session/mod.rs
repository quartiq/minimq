mod drive;
mod handshake;
mod inbound;
mod operations;
mod state;

#[cfg(test)]
mod tests;

use crate::de::PacketReader;
use crate::packets::Disconnect;
use crate::publication::{Publication, ToPayload};
use crate::ser::MAX_FIXED_HEADER_SIZE;
use crate::types::{Auth, TopicFilter};
use crate::{ConfigBuilder, Error, InboundPublish, Op, Property, PubError, QoS, Will};
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
    /// Whether the transport currently owned by an outstanding [`Connection`] is still usable.
    /// Set true by a successful [`connect`](Self::connect) and latched false the moment any
    /// operation observes a fatal transport- or protocol-level disconnect. State carried to the
    /// next connection is reset in `connect`, not here, so the latch does not depend on the
    /// handle's `Drop` running.
    connected: bool,
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
            connected: false,
        }
    }

    /// Latch the current transport as disconnected. Called at every site that observes a fatal
    /// transport- or protocol-level failure. Only flips the liveness flag; state needed for the
    /// next connection is reset in `connect`.
    pub(super) fn mark_disconnected(&mut self) {
        self.connected = false;
    }

    /// Return the maximum inbound MQTT packet size accepted by this session.
    pub fn max_rx_packet_size(&self) -> usize {
        self.packet_reader.capacity()
    }

    /// Return the maximum outbound MQTT packet arena size available to this session.
    pub fn max_tx_packet_size(&self) -> usize {
        self.data.outbound.capacity()
    }

    /// Return whether the session is connected and currently has the local capacity to attempt a
    /// publish at the requested QoS.
    pub fn can_publish(&self, qos: QoS) -> bool {
        self.connected
            && if qos == QoS::AtMostOnce {
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
/// are reachable through the [`Deref`](core::ops::Deref) to the underlying [`Session`].
///
/// Dropping (or [`mem::forget`](core::mem::forget)-ing) the handle releases the transport and the
/// session borrow; the same `Session` can then be reconnected with a fresh transport — possibly of
/// a different `IO` type or buffer lifetime. State needed across reconnects (in-flight QoS replay,
/// keepalive timers, the inbound packet reader) is reset at the start of the next
/// [`Session::connect`](Session::connect), so correctness does not depend on this handle's `Drop`
/// running.
///
/// Note that dropping or forgetting the handle is an *ungraceful* MQTT close: **no `DISCONNECT`
/// packet is sent** (a sync `Drop` cannot perform the async write), so the broker sees an abnormal
/// disconnect and will publish the configured Will, if any. Whether the underlying transport is
/// actually closed is up to the `IO`'s own `Drop`. For a clean shutdown — `DISCONNECT` sent, Will
/// suppressed — call [`disconnect`](Self::disconnect) (or [`disconnect_with`](Self::disconnect_with))
/// before dropping. Reconnecting afterwards with [`Session::connect`](Session::connect) resumes the
/// broker session via `CONNECT` `clean_start=false`; see [`ConnectEvent`](crate::ConnectEvent).
pub struct Connection<'a, 'buf, IO> {
    pub(super) session: &'a mut Session<'buf>,
    pub(super) io: IO,
    pub(super) event: ConnectEvent,
}

impl<'buf, IO> core::ops::Deref for Connection<'_, 'buf, IO> {
    type Target = Session<'buf>;

    fn deref(&self) -> &Self::Target {
        self.session
    }
}

impl<'buf, IO: Io> Connection<'_, 'buf, IO> {
    /// Whether this connection started a fresh broker session or resumed an existing one.
    pub fn connect_event(&self) -> ConnectEvent {
        self.event
    }

    /// Whether the connection is still live.
    ///
    /// Becomes `false` once any operation on this handle has observed a transport- or
    /// protocol-level disconnect (broker `DISCONNECT`, transport error, keepalive timeout, fatal
    /// protocol violation) or after a graceful [`disconnect`](Self::disconnect). Once `false`,
    /// further operations fail fast with [`Error::Disconnected`](crate::Error::Disconnected)
    /// instead of driving the dead transport; drop the handle and `connect` again to recover.
    pub fn is_connected(&self) -> bool {
        self.session.connected
    }

    /// See [`Session`] docs: advance local session state cooperatively without waiting on new
    /// inbound reads or future deadlines.
    pub async fn drive(&mut self) -> Result<Option<InboundPublish<'_>>, Error<IO::Error>> {
        self.session.drive(&mut self.io).await
    }

    /// Wait until any session progress happens or the session disconnects.
    pub async fn poll(&mut self) -> Result<Option<InboundPublish<'_>>, Error<IO::Error>> {
        self.session.poll(&mut self.io).await
    }

    /// Wait until the next inbound publish arrives.
    pub async fn recv(&mut self) -> Result<InboundPublish<'_>, Error<IO::Error>> {
        self.session.recv(&mut self.io).await
    }

    /// Send a `SUBSCRIBE`.
    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<Op, Error<IO::Error>> {
        self.session
            .subscribe(&mut self.io, topics, properties)
            .await
    }

    /// Send an `UNSUBSCRIBE`.
    pub async fn unsubscribe(
        &mut self,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<Op, Error<IO::Error>> {
        self.session
            .unsubscribe(&mut self.io, topics, properties)
            .await
    }

    /// Send a `PUBLISH`.
    pub async fn publish<P>(
        &mut self,
        publication: Publication<'_, P>,
    ) -> Result<Option<Op>, PubError<P::Error, IO::Error>>
    where
        P: ToPayload,
    {
        self.session.publish(&mut self.io, publication).await
    }

    /// Gracefully close the transport with `DISCONNECT`.
    ///
    /// This is the graceful counterpart to simply dropping the handle: it sends the MQTT
    /// `DISCONNECT` so the broker closes cleanly and suppresses the Will. Just dropping (or
    /// [`mem::forget`](core::mem::forget)-ing) the handle skips this and is treated by the broker
    /// as an abnormal disconnect.
    ///
    /// Takes `&mut self` rather than consuming the handle, so a cancelled disconnect can be
    /// retried. After a successful disconnect the handle's transport is dead; drop it (or
    /// `mem::forget` it) to release the session for a fresh `connect`.
    pub async fn disconnect(&mut self) -> Result<(), Error<IO::Error>> {
        self.session
            .disconnect_with(&mut self.io, Disconnect::success())
            .await
    }

    /// Close the transport with a caller-specified `DISCONNECT`.
    pub async fn disconnect_with(
        &mut self,
        disconnect: Disconnect<'_>,
    ) -> Result<(), Error<IO::Error>> {
        self.session.disconnect_with(&mut self.io, disconnect).await
    }
}
