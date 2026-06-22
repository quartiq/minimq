use super::state::ROUND_TRIP_TIMEOUT_MS;
use crate::ser::MAX_FIXED_HEADER_SIZE;
use crate::{Buffers, ConfigBuilder, Publication, tests::block_on};
use crate::{ConnectEvent, Connection, Error, QoS, ResourceError, Session};
use embassy_time::{Duration, Instant};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use std::collections::VecDeque;
use std::vec::Vec;

#[derive(Default)]
struct MockConnection {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
    write_error: Option<ErrorKind>,
}

impl MockConnection {
    fn push_rx(&mut self, data: &[u8]) {
        self.rx.push_back(data.to_vec());
    }
}

impl ErrorType for MockConnection {
    type Error = ErrorKind;
}

impl Read for MockConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let Some(mut chunk) = self.rx.pop_front() else {
            return Err(ErrorKind::TimedOut);
        };
        let len = buf.len().min(chunk.len());
        buf[..len].copy_from_slice(&chunk[..len]);
        if len < chunk.len() {
            chunk.drain(..len);
            self.rx.push_front(chunk);
        }
        Ok(len)
    }
}

impl Write for MockConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if let Some(err) = self.write_error.take() {
            return Err(err);
        }
        self.tx.push(buf.to_vec());
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn session() -> Session<'static> {
    let rx = Box::leak(Box::new([0; 128]));
    let tx = Box::leak(Box::new([0; 1152]));
    Session::new(
        ConfigBuilder::new(Buffers::new(rx, tx))
            .client_id("test")
            .unwrap()
            .keepalive_interval(1),
    )
}

/// Build a live [`Connection`] over a mock transport, bypassing the `connect` handshake, for the
/// white-box tests below that drive the internal connection methods directly. Kept separate from
/// `session()` so that helper still models the genuinely-disconnected `Session::new` default that
/// the `connect`-path and pure-state tests rely on.
fn live_connection<'a>(
    session: &'a mut Session<'static>,
    io: MockConnection,
) -> Connection<'a, 'static, MockConnection> {
    Connection {
        session,
        io,
        event: ConnectEvent::Connected,
        live: true,
    }
}

#[test]
fn session_exposes_local_packet_capacities() {
    let session = session();

    assert_eq!(session.max_rx_packet_size(), 128);
    assert_eq!(session.max_tx_packet_size(), 1152);
}

#[test]
fn maintain_sends_pingreq_when_due() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());
    let now = Instant::now();
    conn.session.runtime.next_ping = Some(now);

    block_on(conn.service(now)).unwrap();

    assert!(
        conn.io
            .tx
            .iter()
            .any(|frame| frame.as_slice() == [0xC0, 0x00])
    );
    assert_eq!(
        conn.session.runtime.ping_timeout,
        Some(now + Duration::from_millis(ROUND_TRIP_TIMEOUT_MS))
    );
    assert_eq!(
        conn.session.runtime.next_ping,
        Some(now + conn.session.runtime.keepalive_send_interval().unwrap())
    );
}

#[test]
fn long_keepalive_schedules_ping_before_expiry() {
    let mut session = session();
    let now = Instant::now();
    session.runtime.keepalive_interval = Duration::from_secs(30);

    session.runtime.note_outbound_activity(now);

    assert_eq!(
        session.runtime.next_ping,
        Some(now + Duration::from_secs(25))
    );
}

#[test]
fn maintain_does_not_send_second_pingreq_while_waiting_for_pingresp() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());
    let now = Instant::now();
    conn.session.runtime.next_ping = Some(now);

    block_on(conn.service(now)).unwrap();
    block_on(conn.service(now + Duration::from_millis(600))).unwrap();

    let pingreqs = conn
        .io
        .tx
        .iter()
        .filter(|frame| frame.as_slice() == [0xC0, 0x00])
        .count();
    assert_eq!(pingreqs, 1);
}

#[test]
fn pingresp_clears_keepalive_timeout() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());
    let now = Instant::now();
    conn.session.runtime.next_ping = Some(now);

    block_on(conn.service(now)).unwrap();
    conn.io.push_rx(&[0xD0, 0x00]);

    block_on(conn.read_packet()).unwrap();
    let result = conn.process_received_packet().unwrap();
    assert!(result.is_none());
    assert_eq!(conn.session.runtime.ping_timeout, None);
    assert!(conn.session.runtime.next_ping.is_some());
}

#[test]
fn drive_returns_none_when_waiting_for_read() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());

    let result = block_on(conn.drive()).unwrap();

    assert!(result.is_none());
}

#[test]
fn poll_returns_none_after_internal_progress() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());
    conn.session.runtime.next_ping = Some(Instant::now());

    let result = block_on(conn.poll()).unwrap();

    assert!(result.is_none());
    assert!(
        conn.io
            .tx
            .iter()
            .any(|frame| frame.as_slice() == [0xC0, 0x00])
    );
}

#[test]
fn inbound_publish_does_not_refresh_keepalive_deadline() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());
    let now = Instant::now();
    let deadline = now + Duration::from_secs(1);
    conn.session.runtime.next_ping = Some(deadline);
    conn.io.push_rx(&[0x30, 0x05, 0x00, 0x01, b'A', 0x00, 0x05]);

    let result = block_on(conn.poll())
        .unwrap()
        .expect("expected inbound publish");
    assert_eq!(result.topic(), "A", "{result:?}");
    assert_eq!(conn.session.runtime.next_ping, Some(deadline));
}

#[test]
fn qos0_publish_refreshes_keepalive_deadline() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());
    let now = Instant::now();
    conn.session.runtime.next_ping = Some(now);

    block_on(conn.publish(Publication::bytes("A", b"5"))).unwrap();
    assert!(conn.session.runtime.next_ping.is_some_and(
        |deadline| deadline >= now + conn.session.runtime.keepalive_send_interval().unwrap()
    ));
}

#[test]
fn expired_ping_timeout_disconnects_session() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());
    let now = Instant::now();
    conn.session.runtime.ping_timeout = Some(now);

    let result = block_on(conn.service(now));

    // The session signals the dead connection by surfacing `Disconnected`; the caller drops the
    // transport. Carried-over state is reset on the next `connect`, not here.
    assert!(matches!(result, Err(Error::Disconnected)));
}

#[test]
fn pingreq_write_error_disconnects_session() {
    let mut session = session();
    let mut conn = live_connection(
        &mut session,
        MockConnection {
            write_error: Some(ErrorKind::ConnectionReset),
            ..Default::default()
        },
    );
    let now = Instant::now();
    conn.session.runtime.next_ping = Some(now);

    let result = block_on(conn.service(now));

    assert!(matches!(
        result,
        Err(Error::Transport(ErrorKind::ConnectionReset))
    ));
}

#[test]
fn connect_uses_tx_buffer_when_rx_only_covers_connack() {
    let rx = Box::leak(Box::new([0; 8]));
    let tx = Box::leak(Box::new([0; 128]));
    let mut session = Session::new(
        ConfigBuilder::new(Buffers::new(rx, tx))
            .client_id("0123456789abcdef")
            .unwrap(),
    );
    let mut connection = MockConnection::default();
    connection.push_rx(&[0x20, 0x03, 0x00, 0x00, 0x00]);

    let connection = block_on(session.connect(connection)).unwrap();

    assert!(matches!(
        connection.connect_event(),
        ConnectEvent::Connected
    ));
    assert_eq!(connection.io.tx.len(), 1);
    assert!(connection.io.tx[0].len() > rx.len());
}

#[test]
fn connect_returns_insufficient_memory_when_tx_is_too_small() {
    let rx = Box::leak(Box::new([0; 8]));
    let tx = Box::leak(Box::new([0; MAX_FIXED_HEADER_SIZE - 1]));
    let mut session = Session::new(
        ConfigBuilder::new(Buffers::new(rx, tx))
            .client_id("test")
            .unwrap(),
    );
    let connection = MockConnection::default();

    let result = block_on(session.connect(connection)).map(|_| ());

    assert!(matches!(
        result,
        Err(Error::Resource(ResourceError::BufferTooSmall))
    ));
}

#[test]
fn timed_out_read_disconnects_session() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());

    let result = block_on(conn.poll());

    assert!(matches!(result, Err(Error::Transport(ErrorKind::TimedOut))));
}

#[test]
fn can_publish_false_after_disconnect() {
    let mut session = session();
    let mut conn = live_connection(&mut session, MockConnection::default());
    assert!(conn.can_publish(QoS::AtMostOnce));
    assert!(conn.can_publish(QoS::AtLeastOnce));

    conn.handle_disconnect();

    assert!(!conn.can_publish(QoS::AtMostOnce));
    assert!(!conn.can_publish(QoS::AtLeastOnce));
    assert!(!conn.can_publish(QoS::ExactlyOnce));
}

#[test]
fn can_publish_gates_qos1_2_on_send_quota() {
    let mut session = session();
    let conn = live_connection(&mut session, MockConnection::default());
    conn.session.runtime.send_quota = 0;

    assert!(!conn.can_publish(QoS::AtLeastOnce));
    assert!(!conn.can_publish(QoS::ExactlyOnce));
    // QoS0 ignores send_quota.
    assert!(conn.can_publish(QoS::AtMostOnce));
}

#[test]
fn can_publish_qos1_false_when_retained_slots_full() {
    let mut session = session();
    let conn = live_connection(&mut session, MockConnection::default());
    for packet_id in 1u16..=8 {
        let offset = (packet_id - 1) * 4;
        conn.session
            .data
            .outbound
            .retain_packet(packet_id, offset as usize, 4)
            .unwrap();
    }

    assert_ne!(conn.session.runtime.send_quota, 0);
    assert!(!conn.can_publish(QoS::AtLeastOnce));
    assert!(!conn.can_publish(QoS::ExactlyOnce));
    // QoS0 only needs scratch space, which is still ample.
    assert!(conn.can_publish(QoS::AtMostOnce));
}
