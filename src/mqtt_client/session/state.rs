use super::*;

pub(super) const PING_TIMEOUT_MS: u64 = 5_000;
const MAX_INBOUND_QOS2: usize = 8;

#[derive(Debug)]
pub(super) struct RuntimeState {
    pub(super) session_resumed: bool,
    pub(super) keepalive_interval: Duration,
    pub(super) send_quota: u16,
    pub(super) max_send_quota: u16,
    pub(super) maximum_packet_size: Option<u32>,
    pub(super) max_qos: Option<QoS>,
    pub(super) next_ping: Option<Instant>,
    pub(super) ping_timeout: Option<Instant>,
}

impl RuntimeState {
    pub(super) fn new(keepalive_interval: Duration) -> Self {
        Self {
            session_resumed: false,
            keepalive_interval,
            send_quota: u16::MAX,
            max_send_quota: u16::MAX,
            maximum_packet_size: None,
            max_qos: None,
            next_ping: None,
            ping_timeout: None,
        }
    }

    pub(super) fn reset_transport(&mut self) {
        self.session_resumed = false;
        self.next_ping = None;
        self.ping_timeout = None;
    }

    pub(super) fn note_outbound_activity(&mut self, now: Instant) {
        self.next_ping = self
            .keepalive_send_interval()
            .map(|interval| now + interval);
    }

    pub(super) fn require_packet_size<E>(&self, len: usize) -> Result<(), Error<E>> {
        if self
            .maximum_packet_size
            .is_some_and(|max| len > max as usize)
        {
            return Err(Error::Protocol(ProtocolError::Failed(
                ReasonCode::PacketTooLarge,
            )));
        }
        Ok(())
    }

    pub(super) fn keepalive_send_interval(&self) -> Option<Duration> {
        let keepalive_ms = self.keepalive_interval.as_millis();
        if keepalive_ms == 0 {
            return None;
        }

        let lead_ms = PING_TIMEOUT_MS.min(keepalive_ms / 2);
        Some(Duration::from_millis(keepalive_ms - lead_ms))
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        match (self.next_ping, self.ping_timeout) {
            (Some(next_ping), Some(ping_timeout)) => Some(next_ping.min(ping_timeout)),
            (Some(next_ping), None) => Some(next_ping),
            (None, Some(ping_timeout)) => Some(ping_timeout),
            (None, None) => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct SessionData<'a> {
    packet_id: NonZeroU16,
    pub(super) outbound: Outbound<'a>,
    pub(super) pending_server_packet_ids: Vec<u16, MAX_INBOUND_QOS2>,
    pub(super) session_present: bool,
}

impl<'a> SessionData<'a> {
    pub(super) fn new(outbound: &'a mut [u8]) -> Self {
        Self {
            packet_id: NonZeroU16::new(1).unwrap(),
            outbound: Outbound::new(outbound),
            pending_server_packet_ids: Vec::new(),
            session_present: false,
        }
    }

    pub(super) fn register_connected(&mut self) {
        self.session_present = true;
    }

    pub(super) fn reset(&mut self) {
        self.session_present = false;
        self.packet_id = NonZeroU16::new(1).unwrap();
        self.outbound.clear();
        self.pending_server_packet_ids.clear();
    }

    pub(super) fn next_packet_id(&mut self) -> u16 {
        let packet_id = self.packet_id.get();
        self.packet_id =
            NonZeroU16::new(packet_id.wrapping_add(1)).unwrap_or(NonZeroU16::new(1).unwrap());
        packet_id
    }
}
