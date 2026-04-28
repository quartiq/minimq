use super::state::PING_TIMEOUT_MS;
use super::*;
use crate::{Buffers, ConfigBuilder, Publication, tests::block_on};
use embassy_time::Duration;
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

fn session() -> Session<'static, MockConnection> {
    let rx = Box::leak(Box::new([0; 128]));
    let tx = Box::leak(Box::new([0; 1152]));
    Session::new(
        ConfigBuilder::new(Buffers::new(rx, tx))
            .client_id("test")
            .unwrap()
            .keepalive_interval(1),
    )
}

#[test]
fn maintain_sends_pingreq_when_due() {
    let mut session = session();
    session.connection = Some(MockConnection::default());
    let now = Instant::now();
    session.runtime.next_ping = Some(now);

    block_on(session.maintain(now)).unwrap();

    let connection = session.connection.as_ref().unwrap();
    assert!(
        connection
            .tx
            .iter()
            .any(|frame| frame.as_slice() == [0xC0, 0x00])
    );
    assert_eq!(
        session.runtime.ping_timeout,
        Some(now + Duration::from_millis(PING_TIMEOUT_MS))
    );
    assert_eq!(
        session.runtime.next_ping,
        Some(now + session.runtime.keepalive_send_interval().unwrap())
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
    session.connection = Some(MockConnection::default());
    let now = Instant::now();
    session.runtime.next_ping = Some(now);

    block_on(session.maintain(now)).unwrap();
    block_on(session.maintain(now + Duration::from_millis(600))).unwrap();

    let pingreqs = session
        .connection
        .as_ref()
        .unwrap()
        .tx
        .iter()
        .filter(|frame| frame.as_slice() == [0xC0, 0x00])
        .count();
    assert_eq!(pingreqs, 1);
}

#[test]
fn pingresp_clears_keepalive_timeout() {
    let mut session = session();
    session.connection = Some(MockConnection::default());
    let now = Instant::now();
    session.runtime.next_ping = Some(now);

    block_on(session.maintain(now)).unwrap();
    session.connection.as_mut().unwrap().push_rx(&[0xD0, 0x00]);

    block_on(session.read_packet()).unwrap();
    let result = session.handle_received_packet().unwrap();
    assert!(result.is_none());
    assert_eq!(session.runtime.ping_timeout, None);
    assert!(session.runtime.next_ping.is_some());
}

#[test]
fn inbound_publish_does_not_refresh_keepalive_deadline() {
    let mut session = session();
    session.connection = Some(MockConnection::default());
    let now = Instant::now();
    let deadline = now + Duration::from_secs(1);
    session.runtime.next_ping = Some(deadline);
    session
        .connection
        .as_mut()
        .unwrap()
        .push_rx(&[0x30, 0x05, 0x00, 0x01, b'A', 0x00, 0x05]);

    let result = block_on(session.poll()).unwrap();
    assert!(matches!(result, Event::Inbound(_)), "{result:?}");
    assert_eq!(session.runtime.next_ping, Some(deadline));
}

#[test]
fn qos0_publish_refreshes_keepalive_deadline() {
    let mut session = session();
    session.connection = Some(MockConnection::default());
    let now = Instant::now();
    session.runtime.next_ping = Some(now);

    block_on(session.publish(Publication::bytes("A", b"5"))).unwrap();
    assert!(session.runtime.next_ping.is_some_and(
        |deadline| deadline >= now + session.runtime.keepalive_send_interval().unwrap()
    ));
}

#[test]
fn expired_ping_timeout_disconnects_session() {
    let mut session = session();
    session.connection = Some(MockConnection::default());
    let now = Instant::now();
    session.runtime.ping_timeout = Some(now);

    let result = block_on(session.maintain(now));

    assert!(matches!(result, Err(Error::Disconnected)));
    assert!(session.connection.is_none());
    assert_eq!(session.runtime.ping_timeout, None);
    assert_eq!(session.runtime.next_ping, None);
}

#[test]
fn pingreq_write_error_disconnects_session() {
    let mut session = session();
    session.connection = Some(MockConnection {
        write_error: Some(ErrorKind::ConnectionReset),
        ..Default::default()
    });
    let now = Instant::now();
    session.runtime.next_ping = Some(now);

    let result = block_on(session.maintain(now));

    assert!(matches!(
        result,
        Err(Error::Transport(ErrorKind::ConnectionReset))
    ));
    assert!(session.connection.is_none());
    assert_eq!(session.runtime.next_ping, None);
    assert_eq!(session.runtime.ping_timeout, None);
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

    let result = block_on(session.connect(connection));

    assert!(matches!(result, Ok(ConnectEvent::Connected)));
    let connection = session.connection.as_ref().unwrap();
    assert_eq!(connection.tx.len(), 1);
    assert!(connection.tx[0].len() > rx.len());
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

    let result = block_on(session.connect(connection));

    assert!(matches!(
        result,
        Err(Error::Protocol(ProtocolError::Encode(
            crate::SerError::InsufficientMemory
        )))
    ));
}

#[test]
fn timed_out_read_disconnects_session() {
    let mut session = session();
    session.connection = Some(MockConnection::default());

    let result = block_on(session.poll());

    assert!(matches!(result, Err(Error::Transport(ErrorKind::TimedOut))));
    assert!(session.connection.is_none());
}
