#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), no_std)]

use defmt::{debug, info, trace, warn};
use heapless::String;
use minimq::{
    ConnectEvent, Connection, Error as MqttError, InboundPublish, Io, Op, Properties, Property,
    PubError, Publication, QoS, ResourceError, RetainHandling, SubscriptionOptions, TopicFilter,
};
use serde::{Deserialize, Serialize};

type TopicString = String<128>;

const MAX_TRANSFER_ID_BYTES: usize = 48;
const MAX_STATUS_BYTES: usize = 256;
const INFO_CHUNK_STRIDE: usize = 128;
const MANIFEST_SUFFIX: &str = "/manifest";
const STATUS_SUFFIX: &str = "/status";
const CHUNK_SUFFIX: &str = "/chunk";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Transfer {
    id: String<MAX_TRANSFER_ID_BYTES>,
    size: u32,
    next_offset: u32,
    fnv1a64: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest<'a> {
    id: &'a str,
    size: u32,
    fnv1a64: u64,
}

/// Immediate result of one cooperative `step()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "inspect whether staging work is still pending"]
pub enum Step {
    /// No queued staging work remains after this step.
    Quiescent,
    /// Staging still has queued or in-flight MQTT work.
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Error returned while constructing a staging service.
pub enum CreateError {
    /// A derived protocol topic does not fit the fixed topic buffer.
    Topic(ResourceError),
    /// The maximum chunk size or storage write size cannot support aligned writes.
    InvalidChunkSize,
}

/// Storage limits for one staging service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Maximum staged object size in bytes.
    pub capacity: u32,
    /// Maximum bytes accepted in one MQTT chunk.
    pub max_chunk_size: usize,
    /// Required storage-write alignment in bytes.
    pub write_size: usize,
}

impl core::fmt::Display for CreateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Topic(_) => f.write_str("staging topic does not fit buffer"),
            Self::InvalidChunkSize => f.write_str("invalid staging chunk or write size"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, defmt::Format)]
#[serde(rename_all = "kebab-case")]
/// Current staging phase.
pub enum State {
    /// No transfer is active.
    Idle,
    /// The storage backend is preparing the staging area.
    Preparing,
    /// The device is ready for the next chunk.
    Ready,
    /// The storage backend is writing one chunk.
    Writing,
    /// The full object was staged.
    Complete,
    /// The transfer failed or was aborted.
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, defmt::Format)]
#[serde(rename_all = "kebab-case")]
/// Result conveyed by a staging status publication.
pub enum StatusCode {
    /// No transfer is active.
    Idle,
    /// The manifest or latest chunk was accepted.
    Accepted,
    /// An already accepted chunk was received again.
    Duplicate,
    /// The chunk did not match the requested offset.
    Offset,
    /// The object or chunk exceeds a configured limit.
    Oversize,
    /// The MQTT RX packet budget cannot carry a configured chunk.
    Mtu,
    /// The chunk does not meet the storage write alignment.
    Unaligned,
    /// The storage backend rejected an operation.
    Storage,
    /// The object was staged.
    Complete,
    /// The request or service state was invalid.
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
/// Current staging progress and device limits.
pub struct Status<'a> {
    /// Current transfer state.
    pub state: State,
    /// Result associated with this status publication.
    pub code: StatusCode,
    /// Manifest identifier, or an empty string while idle.
    pub id: &'a str,
    /// First object byte not yet written.
    pub next_offset: u32,
    /// Declared object size in bytes.
    pub size: u32,
    /// Maximum chunk payload accepted by this service.
    pub mtu: usize,
    /// Required chunk alignment in bytes.
    pub write_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Publish(StatusCode),
    Subscribe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkError {
    Queue(StatusCode),
    Fail(StatusCode),
}

struct InFlightAction {
    action: Action,
    op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingRequest {
    Prepare,
    Write { next_offset: u32, final_chunk: bool },
}

/// Storage operation requested by the staging state machine.
#[derive(Debug)]
pub enum StagingRequest<'a> {
    /// Prepare the staging area for one object.
    Prepare {
        /// Declared object size in bytes.
        size: u32,
    },
    /// Write one accepted chunk and optionally finalize the object.
    Write(StagingWrite<'a>),
}

/// One accepted chunk ready for storage.
#[derive(Debug)]
pub struct StagingWrite<'a> {
    /// Object byte offset.
    pub offset: u32,
    /// Chunk bytes borrowed from the current Minimq inbound publish. The
    /// owner must consume or copy them before polling the connection again.
    pub payload: &'a [u8],
    /// Manifest size, excluding any final erased-storage padding.
    pub size: u32,
    /// Expected FNV-1a checksum. Present only on the final chunk.
    pub fnv1a64: Option<u64>,
}

/// Result of handling one inbound MQTT publish.
#[must_use = "submit storage work or route unhandled traffic"]
#[derive(Debug)]
pub enum Handle<'a> {
    /// The publish is not staging traffic.
    Unhandled,
    /// Staging traffic was consumed without new storage work.
    Consumed,
    /// The owner must execute this storage request and report its result with
    /// `Service::complete_request`.
    Request(StagingRequest<'a>),
}

/// Cooperative MQTT staging service.
pub struct Service {
    prefix: TopicString,
    inflight: Option<InFlightAction>,
    pending: Option<Action>,
    startup_publish: Option<StatusCode>,
    state: State,
    transfer: Option<Transfer>,
    request: Option<PendingRequest>,
    capacity: u32,
    max_chunk_size: usize,
    max_rx_packet_size: usize,
    write_size: usize,
}

impl Service {
    /// Create a staging service at the exact protocol topic root supplied by the
    /// application.
    pub fn new(prefix: &str, config: Config) -> Result<Self, CreateError> {
        if config.write_size == 0
            || config.max_chunk_size == 0
            || !config.max_chunk_size.is_multiple_of(config.write_size)
        {
            return Err(CreateError::InvalidChunkSize);
        }
        let prefix = TopicString::try_from(prefix)
            .map_err(|_| CreateError::Topic(ResourceError::BufferTooSmall))?;
        let _ = topic(&prefix, MANIFEST_SUFFIX).map_err(CreateError::Topic)?;
        let _ = topic(&prefix, STATUS_SUFFIX).map_err(CreateError::Topic)?;
        let _ = topic(&prefix, CHUNK_SUFFIX).map_err(CreateError::Topic)?;
        Ok(Self {
            prefix,
            inflight: None,
            pending: None,
            startup_publish: None,
            state: State::Idle,
            transfer: None,
            request: None,
            capacity: config.capacity,
            max_chunk_size: config.max_chunk_size,
            max_rx_packet_size: 0,
            write_size: config.write_size,
        })
    }

    /// Begin staging startup for one MQTT connect event.
    ///
    /// Pure local state update. Cancel-safe.
    pub fn begin_startup(&mut self, event: ConnectEvent) {
        let replay = self.replay_status();
        match event {
            ConnectEvent::Connected => {
                self.inflight = None;
                self.startup_publish = replay;
                self.queue(Action::Subscribe);
            }
            ConnectEvent::Reconnected => {
                self.startup_publish = None;
                if let Some(code) = replay {
                    self.queue(Action::Publish(code));
                }
            }
        }
    }

    /// Return the current staging status.
    ///
    /// Pure query. Cancel-safe.
    pub fn status(&self) -> Status<'_> {
        self.status_with(self.current_code())
    }

    /// Return whether staging has reached a terminal complete state and there is
    /// no remaining queued or in-flight MQTT work.
    pub fn is_complete(&self) -> bool {
        self.state == State::Complete
            && self.request.is_none()
            && self.pending.is_none()
            && self.inflight.is_none()
            && self.startup_publish.is_none()
    }

    fn current_code(&self) -> StatusCode {
        match self.state {
            State::Idle => StatusCode::Idle,
            State::Preparing | State::Ready | State::Writing => StatusCode::Accepted,
            State::Complete => StatusCode::Complete,
            State::Error => StatusCode::Error,
        }
    }

    fn status_with(&self, code: StatusCode) -> Status<'_> {
        let transfer = self.transfer.as_ref();
        Status {
            state: self.state,
            code,
            id: transfer.map_or("", |transfer| transfer.id.as_str()),
            next_offset: transfer.map_or(0, |transfer| transfer.next_offset),
            size: transfer.map_or(0, |transfer| transfer.size),
            mtu: self.max_chunk_size,
            write_size: self.write_size,
        }
    }

    /// Abort an active staging transfer.
    ///
    /// Pure local state update. Cancel-safe.
    ///
    /// Returns `true` if a staging transfer was active and is now marked as
    /// failed. A later `step()` or reconnect startup replay will publish
    /// `error` status for the same transfer.
    pub fn abort(&mut self) -> bool {
        if !matches!(self.state, State::Preparing | State::Ready | State::Writing) {
            return false;
        }
        warn!(
            "Aborting staging transfer state={:?} offset={=u32} size={=u32}",
            self.state,
            self.status().next_offset,
            self.status().size
        );
        self.state = State::Error;
        self.request = None;
        if matches!(self.inflight.as_ref(), Some(inflight) if inflight.action == Action::Subscribe)
            || self.pending == Some(Action::Subscribe)
        {
            self.startup_publish = Some(StatusCode::Error);
        } else {
            self.queue(Action::Publish(StatusCode::Error));
        }
        true
    }

    /// Handle one inbound publish.
    ///
    /// Pure local state update returning chunk data borrowed from `inbound`.
    /// This is cancel-safe.
    pub fn handle<'a>(&mut self, inbound: &InboundPublish<'a>) -> Handle<'a> {
        self.handle_publish_with_properties(
            inbound.topic(),
            inbound.payload(),
            inbound.properties(),
        )
    }

    /// Complete one previously emitted storage request.
    ///
    /// Pure local state update. Cancel-safe.
    pub fn complete_request(&mut self, success: bool) {
        let Some(request) = self.request.take() else {
            return;
        };
        let Some(transfer) = self.transfer.as_mut() else {
            self.state = State::Error;
            self.queue(Action::Publish(StatusCode::Error));
            return;
        };
        if !success {
            warn!(
                "Staging storage step failed state={:?} offset={=u32} size={=u32}",
                self.state, transfer.next_offset, transfer.size
            );
            self.state = State::Error;
            self.queue(Action::Publish(StatusCode::Storage));
            return;
        }
        match request {
            PendingRequest::Prepare => {
                self.state = State::Ready;
                info!("Accepted staging manifest size={=u32}", transfer.size);
                self.queue(Action::Publish(StatusCode::Accepted));
            }
            PendingRequest::Write {
                next_offset,
                final_chunk,
                ..
            } => {
                if final_chunk {
                    transfer.next_offset = transfer.size;
                    self.state = State::Complete;
                    info!("Completed staging object size={=u32}", transfer.size);
                    self.queue(Action::Publish(StatusCode::Complete));
                } else {
                    debug!(
                        "staging chunk write completed next_offset={=u32} size={=u32}",
                        next_offset, transfer.size
                    );
                    transfer.next_offset = next_offset;
                    self.state = State::Ready;
                    self.queue(Action::Publish(StatusCode::Accepted));
                }
            }
        }
    }

    fn handle_publish_with_properties<'a>(
        &mut self,
        topic: &str,
        payload: &'a [u8],
        properties: &Properties<'_>,
    ) -> Handle<'a> {
        if topic.strip_prefix(self.prefix.as_str()) == Some(MANIFEST_SUFFIX) {
            debug!("staging manifest received payload={=usize}B", payload.len());
            return self.handle_manifest(payload);
        }
        if topic.strip_prefix(self.prefix.as_str()) != Some(CHUNK_SUFFIX) {
            return Handle::Unhandled;
        }
        let Some(chunk) = chunk_properties(properties) else {
            warn!("Rejecting staging chunk without required properties");
            return Handle::Consumed;
        };
        self.handle_chunk(chunk.id, chunk.offset, payload)
    }

    /// Advance one queued MQTT operation.
    ///
    /// This is the cooperative queue-drain API. It performs at most one local
    /// queued subscribe or status-publish step and does not wait for future
    /// inbound reads on its own.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    /// The current action stays at the front of the local queue until the MQTT
    /// operation is known to have completed or been invalidated.
    pub async fn step<IO>(
        &mut self,
        connection: &mut Connection<'_, '_, IO>,
    ) -> Result<Step, MqttError<IO::Error>>
    where
        IO: Io,
    {
        if let Some(inflight) = self.inflight.take() {
            if connection.is_pending(&inflight.op) {
                self.inflight = Some(inflight);
                return Ok(Step::Pending);
            }
            if connection.is_invalidated(&inflight.op) {
                self.queue(inflight.action);
                return Err(MqttError::Disconnected);
            }
            if inflight.action == Action::Subscribe
                && let Some(code) = self.startup_publish.take()
            {
                self.queue(Action::Publish(code));
            }
        }

        let Some(action) = self.pending.take() else {
            return Ok(Step::Quiescent);
        };
        let op = match self.start_action(connection, action).await {
            Ok(op) => op,
            Err(error) => {
                self.pending = Some(action);
                return Err(error);
            }
        };
        self.inflight = op.map(|op| InFlightAction { action, op });
        Ok(if self.inflight.is_none() && self.pending.is_none() {
            Step::Quiescent
        } else {
            Step::Pending
        })
    }

    fn handle_manifest<'a>(&mut self, payload: &'a [u8]) -> Handle<'a> {
        let Ok(manifest) = parse_manifest(payload) else {
            warn!("Rejecting staging manifest: invalid manifest");
            if self.state == State::Idle {
                self.queue(Action::Publish(StatusCode::Error));
            }
            return Handle::Consumed;
        };
        if self.state != State::Idle {
            if self.transfer.as_ref().is_some_and(|transfer| {
                transfer.id.as_str() == manifest.id
                    && transfer.size == manifest.size
                    && transfer.fnv1a64 == manifest.fnv1a64
            }) {
                debug!("Replaying status for duplicate staging manifest");
                self.queue(Action::Publish(self.current_code()));
                return Handle::Consumed;
            }
            warn!(
                "Rejecting overlapping staging manifest state={:?} offset={=u32}",
                self.state,
                self.status().next_offset
            );
            return Handle::Consumed;
        }
        if manifest.size == 0 {
            warn!("Rejecting staging manifest: zero-length object");
            self.queue(Action::Publish(StatusCode::Error));
            return Handle::Consumed;
        }
        if manifest.size > self.capacity {
            warn!(
                "Rejecting staging manifest: object oversize size={=u32} capacity={=u32}",
                manifest.size, self.capacity
            );
            self.queue(Action::Publish(StatusCode::Oversize));
            return Handle::Consumed;
        }
        if required_rx_bytes(
            self.chunk_topic().as_str(),
            manifest.id.as_bytes(),
            self.max_chunk_size,
        ) > self.max_rx_packet_size
        {
            warn!(
                "Rejecting staging manifest: chunk exceeds MQTT rx budget chunk={=usize} max_rx={=usize}",
                self.max_chunk_size, self.max_rx_packet_size
            );
            self.queue(Action::Publish(StatusCode::Mtu));
            return Handle::Consumed;
        }
        let size = manifest.size;
        self.transfer = Some(manifest);
        self.state = State::Preparing;
        self.queue(Action::Publish(StatusCode::Accepted));
        self.request = Some(PendingRequest::Prepare);
        Handle::Request(StagingRequest::Prepare { size })
    }

    fn handle_chunk<'a>(&mut self, id: &[u8], offset: u32, payload: &'a [u8]) -> Handle<'a> {
        trace!(
            "staging chunk received offset={=u32} len={=usize}",
            offset,
            payload.len()
        );
        let Some(transfer) = self.transfer.as_mut() else {
            debug!("Ignoring staging chunk without active transfer");
            self.queue(Action::Publish(StatusCode::Idle));
            return Handle::Consumed;
        };
        if id != transfer.id.as_bytes() {
            debug!("Ignoring staging chunk for another transfer");
            return Handle::Consumed;
        }
        if self.state == State::Preparing {
            debug!("Ignoring staging chunk while prepare is still pending");
            return Handle::Consumed;
        }
        if matches!(self.state, State::Complete | State::Error) {
            self.queue(Action::Publish(self.current_code()));
            return Handle::Consumed;
        }
        if self.state == State::Writing {
            debug!("Ignoring staging chunk while a write is pending");
            return Handle::Consumed;
        }
        if self.state != State::Ready {
            debug!(
                "Ignoring staging chunk while state={:?} offset={=u32}",
                self.state, transfer.next_offset
            );
            self.queue(Action::Publish(StatusCode::Idle));
            return Handle::Consumed;
        }
        let next_offset = match validate_chunk(
            transfer,
            offset,
            payload,
            self.capacity,
            self.max_chunk_size,
            self.write_size,
        ) {
            Ok(next_offset) => next_offset,
            Err(ChunkError::Queue(code)) => {
                debug!(
                    "Ignoring staging chunk expected={=u32} got={=u32} len={=usize} code={:?}",
                    transfer.next_offset,
                    offset,
                    payload.len(),
                    code
                );
                self.queue(Action::Publish(code));
                return Handle::Consumed;
            }
            Err(ChunkError::Fail(code)) => {
                warn!(
                    "Rejecting staging chunk offset={=u32} len={=usize} code={:?}",
                    offset,
                    payload.len(),
                    code
                );
                self.state = State::Error;
                self.queue(Action::Publish(code));
                return Handle::Consumed;
            }
        };
        let final_chunk = next_offset == transfer.size;
        let fnv1a64 = final_chunk.then_some(transfer.fnv1a64);
        self.state = State::Writing;
        self.request = Some(PendingRequest::Write {
            next_offset,
            final_chunk,
        });
        debug!(
            "Queueing staging write offset={=u32} len={=usize} final={=bool}",
            offset,
            payload.len(),
            final_chunk
        );
        Handle::Request(StagingRequest::Write(StagingWrite {
            offset,
            payload,
            size: transfer.size,
            fnv1a64,
        }))
    }

    fn replay_status(&self) -> Option<StatusCode> {
        (self.state != State::Idle).then(|| self.current_code())
    }

    fn queue(&mut self, action: Action) {
        if matches!(
            (self.inflight.as_ref(), action),
            (Some(inflight), Action::Subscribe)
                if inflight.action == Action::Subscribe
        ) {
            return;
        }
        self.pending = Some(action);
    }

    fn manifest_topic(&self) -> TopicString {
        topic(&self.prefix, MANIFEST_SUFFIX).expect("validated staging manifest topic")
    }

    fn status_topic(&self) -> TopicString {
        topic(&self.prefix, STATUS_SUFFIX).expect("validated staging status topic")
    }

    fn chunk_topic(&self) -> TopicString {
        topic(&self.prefix, CHUNK_SUFFIX).expect("validated staging chunk topic")
    }

    async fn start_action<IO>(
        &mut self,
        connection: &mut Connection<'_, '_, IO>,
        action: Action,
    ) -> Result<Option<Op>, MqttError<IO::Error>>
    where
        IO: Io,
    {
        match action {
            Action::Subscribe => {
                info!("Subscribing staging MQTT topics");
                self.max_rx_packet_size = connection.session().max_rx_packet_size();
                let manifest_topic = self.manifest_topic();
                let chunk_topic = self.chunk_topic();
                let filters = [
                    TopicFilter::new(manifest_topic.as_str()).options(
                        SubscriptionOptions::default()
                            .maximum_qos(QoS::AtLeastOnce)
                            .retain_behavior(RetainHandling::Never)
                            .ignore_local_messages(),
                    ),
                    TopicFilter::new(chunk_topic.as_str()).options(
                        SubscriptionOptions::default()
                            .maximum_qos(QoS::AtLeastOnce)
                            .retain_behavior(RetainHandling::Never)
                            .ignore_local_messages(),
                    ),
                ];
                Ok(Some(connection.subscribe(&filters, &[]).await?))
            }
            Action::Publish(code) => self.publish_status(connection, code).await,
        }
    }

    async fn publish_status<IO>(
        &self,
        connection: &mut Connection<'_, '_, IO>,
        code: StatusCode,
    ) -> Result<Option<Op>, MqttError<IO::Error>>
    where
        IO: Io,
    {
        let status = self.status_with(code);
        let mut payload = [0; MAX_STATUS_BYTES];
        let len = serde_json_core::ser::to_slice(&status, &mut payload)
            .map_err(|_| MqttError::Resource(ResourceError::BufferTooSmall))?;
        let mut offset = itoa::Buffer::new();
        let chunk_topic = self.chunk_topic();
        let id = self
            .transfer
            .as_ref()
            .map_or(&[][..], |transfer| transfer.id.as_bytes());
        let properties = [
            Property::PayloadFormatIndicator(1),
            Property::ResponseTopic(chunk_topic.as_str()),
            Property::CorrelationData(id),
            Property::UserProperty("offset", offset.format(status.next_offset)),
        ];
        match connection
            .publish(
                Publication::new(self.status_topic().as_str(), &payload[..len])
                    .properties(&properties)
                    .qos(QoS::AtLeastOnce),
            )
            .await
        {
            Ok(op) => {
                log_status(code, status);
                Ok(op)
            }
            Err(PubError::Payload(_)) => unreachable!(),
            Err(PubError::Session(err)) => Err(err),
        }
    }
}

fn log_status(code: StatusCode, status: Status<'_>) {
    match code {
        StatusCode::Accepted => {
            if should_log_chunk_progress(status.next_offset, status.size, status.mtu) {
                info!(
                    "staging status code={:?} offset={=u32}",
                    code, status.next_offset
                );
            } else {
                debug!(
                    "staging status code={:?} offset={=u32}",
                    code, status.next_offset
                );
            }
        }
        StatusCode::Complete => {
            info!(
                "staging status code={:?} offset={=u32}",
                code, status.next_offset
            );
        }
        StatusCode::Idle | StatusCode::Duplicate => {
            debug!(
                "staging status code={:?} offset={=u32}",
                code, status.next_offset
            );
        }
        StatusCode::Offset
        | StatusCode::Oversize
        | StatusCode::Mtu
        | StatusCode::Unaligned
        | StatusCode::Storage
        | StatusCode::Error => {
            warn!(
                "staging status code={:?} offset={=u32}",
                code, status.next_offset
            );
        }
    }
}

fn should_log_chunk_progress(next_offset: u32, size: u32, chunk_size: usize) -> bool {
    if next_offset == 0 {
        return true;
    }
    if next_offset + chunk_size as u32 >= size {
        return true;
    }
    (next_offset as usize / chunk_size).is_multiple_of(INFO_CHUNK_STRIDE)
}

fn validate_chunk(
    transfer: &Transfer,
    offset: u32,
    payload: &[u8],
    capacity: u32,
    max_chunk_size: usize,
    write_size: usize,
) -> Result<u32, ChunkError> {
    if offset < transfer.next_offset {
        return Err(ChunkError::Queue(StatusCode::Duplicate));
    }
    if offset != transfer.next_offset {
        return Err(ChunkError::Queue(StatusCode::Offset));
    }

    if payload.is_empty() || payload.len() > max_chunk_size {
        return Err(ChunkError::Fail(StatusCode::Mtu));
    }
    if !payload.len().is_multiple_of(write_size) {
        return Err(ChunkError::Fail(StatusCode::Unaligned));
    }

    let Some(end) = offset.checked_add(payload.len() as u32) else {
        return Err(ChunkError::Fail(StatusCode::Oversize));
    };
    if end > capacity {
        return Err(ChunkError::Fail(StatusCode::Oversize));
    }
    if end <= transfer.size {
        return Ok(end);
    }

    let Some(padding) = payload_padding(payload, transfer.size - offset) else {
        return Err(ChunkError::Fail(StatusCode::Oversize));
    };
    if !padding.iter().all(|&byte| byte == 0xff) {
        return Err(ChunkError::Fail(StatusCode::Oversize));
    }
    Ok(transfer.size)
}

fn parse_manifest(payload: &[u8]) -> Result<Transfer, ()> {
    let (manifest, used) = serde_json_core::from_slice::<Manifest<'_>>(payload).map_err(|_| ())?;
    if !valid_id(manifest.id) || !payload[used..].iter().all(u8::is_ascii_whitespace) {
        return Err(());
    }
    Ok(Transfer {
        id: manifest.id.try_into().map_err(|_| ())?,
        size: manifest.size,
        next_offset: 0,
        fnv1a64: manifest.fnv1a64,
    })
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TRANSFER_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
}

struct ChunkProperties<'a> {
    id: &'a [u8],
    offset: u32,
}

fn chunk_properties<'a>(properties: &'a Properties<'a>) -> Option<ChunkProperties<'a>> {
    let id = properties.correlation_data()?;
    let mut offset = None;
    for property in properties.iter() {
        let Ok(Property::UserProperty(key, value)) = property else {
            continue;
        };
        if key == "offset" {
            offset = value.parse().ok();
        }
    }
    Some(ChunkProperties {
        id,
        offset: offset?,
    })
}

fn required_rx_bytes(topic: &str, id: &[u8], payload_size: usize) -> usize {
    let properties = 2 // PayloadFormatIndicator property and value
        + 1 // CorrelationData property id
        + 2 // binary length prefix
        + id.len()
        + 1 // UserProperty property id
        + 2 // key UTF-8 length prefix
        + "offset".len()
        + 2 // value UTF-8 length prefix
        + 10; // max u32 decimal digits
    let remaining = 2 // topic UTF-8 length prefix
        + topic.len()
        + 2 // QoS 1 packet identifier
        + mqtt_varint_len(properties)
        + properties
        + payload_size;
    1 + mqtt_varint_len(remaining) + remaining
}

fn mqtt_varint_len(mut value: usize) -> usize {
    let mut len = 1;
    while value >= 128 {
        value /= 128;
        len += 1;
    }
    len
}

fn payload_padding(payload: &[u8], object_bytes: u32) -> Option<&[u8]> {
    let object_bytes = usize::try_from(object_bytes).ok()?;
    (object_bytes < payload.len()).then(|| &payload[object_bytes..])
}

fn topic(prefix: &TopicString, suffix: &str) -> Result<TopicString, ResourceError> {
    let mut topic = prefix.clone();
    topic
        .push_str(suffix)
        .map_err(|_| ResourceError::BufferTooSmall)?;
    Ok(topic)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::sync::OnceLock;

    const ID: &[u8] = b"7ea690cc8c2cd8ed";
    const ID_6: &[u8] = b"97463f6d0098b84a";
    const SLOT: usize = 32;

    fn init_host_logging() {
        static HOST_LOGGING: OnceLock<()> = OnceLock::new();

        HOST_LOGGING.get_or_init(|| {
            let _ = env_logger::builder().is_test(true).try_init();
            defmt2log::init_from_current_exe();
        });
    }

    #[derive(Default)]
    struct Backend {
        prepared: Option<u32>,
        writes: std::vec::Vec<(u32, std::vec::Vec<u8>)>,
        finished: Option<u32>,
        fail_finish: bool,
    }

    impl Backend {
        fn apply(&mut self, request: StagingRequest<'_>) -> bool {
            match request {
                StagingRequest::Prepare { size } => {
                    self.prepared = Some(size);
                    true
                }
                StagingRequest::Write(write) => {
                    self.writes.push((write.offset, write.payload.to_vec()));
                    if write.fnv1a64.is_some() {
                        if self.fail_finish {
                            return false;
                        }
                        self.finished = Some(write.size);
                    }
                    true
                }
            }
        }
    }

    fn apply_handle(service: &mut Service, backend: &mut Backend, handle: Handle<'_>) {
        if let Handle::Request(request) = handle {
            service.complete_request(backend.apply(request));
        }
    }

    fn manifest<'a>(service: &mut Service, payload: &'a [u8]) -> Handle<'a> {
        service.handle_publish_with_properties(
            service.manifest_topic().as_str(),
            payload,
            &Properties::from_slice(&[]),
        )
    }

    fn chunk<'a>(service: &mut Service, id: &[u8], offset: &str, payload: &'a [u8]) -> Handle<'a> {
        let properties = [
            Property::CorrelationData(id),
            Property::UserProperty("offset", offset),
        ];
        service.handle_publish_with_properties(
            service.chunk_topic().as_str(),
            payload,
            &Properties::from_slice(&properties),
        )
    }

    fn drive_manifest(service: &mut Service, backend: &mut Backend, payload: &[u8]) {
        let handle = manifest(service, payload);
        apply_handle(service, backend, handle);
    }

    fn drive_chunk(
        service: &mut Service,
        backend: &mut Backend,
        id: &[u8],
        offset: &str,
        payload: &[u8],
    ) {
        let handle = chunk(service, id, offset, payload);
        apply_handle(service, backend, handle);
    }

    fn service(rx: usize) -> Service {
        init_host_logging();
        let mut service = Service::new(
            "devices/example/staging",
            Config {
                capacity: 128,
                max_chunk_size: SLOT,
                write_size: 4,
            },
        )
        .unwrap();
        service.max_rx_packet_size = rx;
        service
    }

    fn ready_service() -> Service {
        let mut service = service(128);
        service.state = State::Ready;
        service.transfer = Some(Transfer {
            id: "7ea690cc8c2cd8ed".try_into().unwrap(),
            size: 64,
            next_offset: 32,
            fnv1a64: 0,
        });
        service
    }

    #[test]
    fn connected_startup_queues_subscribe_only_for_idle_service() {
        let mut service = service(128);
        service.begin_startup(ConnectEvent::Connected);
        assert_eq!(service.pending, Some(Action::Subscribe));
        assert_eq!(service.startup_publish, None);
    }

    #[test]
    fn connected_startup_defers_status_replay_until_after_subscribe() {
        let mut service = ready_service();
        service.begin_startup(ConnectEvent::Connected);
        assert_eq!(service.pending, Some(Action::Subscribe));
        assert_eq!(service.startup_publish, Some(StatusCode::Accepted));
    }

    #[test]
    fn reconnected_startup_replays_current_status_without_subscribe() {
        let mut service = ready_service();
        service.begin_startup(ConnectEvent::Reconnected);
        assert_eq!(service.pending, Some(Action::Publish(StatusCode::Accepted)));
        assert_eq!(service.startup_publish, None);
    }

    #[test]
    fn status_reports_ready_transfer_state() {
        let service = ready_service();
        let status = service.status();
        assert_eq!(status.state, State::Ready);
        assert_eq!(status.code, StatusCode::Accepted);
        assert_eq!(status.id, "7ea690cc8c2cd8ed");
        assert_eq!(status.next_offset, 32);
        assert_eq!(status.size, 64);
        assert_eq!(status.mtu, SLOT);
        assert_eq!(status.write_size, 4);
    }

    #[test]
    fn largest_status_fits_the_fixed_buffer() {
        let status = Status {
            state: State::Preparing,
            code: StatusCode::Unaligned,
            id: "012345678901234567890123456789012345678901234567",
            next_offset: u32::MAX,
            size: u32::MAX,
            mtu: usize::MAX,
            write_size: usize::MAX,
        };
        let mut payload = [0; MAX_STATUS_BYTES];
        assert!(serde_json_core::ser::to_slice(&status, &mut payload).is_ok());
    }

    #[test]
    fn abort_marks_ready_transfer_failed() {
        let mut service = ready_service();
        assert!(service.abort());
        assert_eq!(service.status().state, State::Error);
        assert_eq!(service.status().code, StatusCode::Error);
        assert_eq!(service.pending, Some(Action::Publish(StatusCode::Error)));
    }

    #[test]
    fn abort_is_noop_without_active_transfer() {
        let mut service = service(128);
        assert!(!service.abort());
        assert_eq!(service.status().state, State::Idle);
    }

    #[test]
    fn abort_is_noop_after_transfer_completes() {
        let mut service = service(128);
        service.state = State::Complete;
        assert!(!service.abort());
        assert_eq!(service.status().state, State::Complete);
    }

    #[test]
    fn manifest_reports_preparing_before_storage_prepare_completes() {
        let mut service = service(128);
        let handle = manifest(
            &mut service,
            br#"{"id":"7ea690cc8c2cd8ed","size":8,"fnv1a64":9126140903112366317}"#,
        );
        assert!(matches!(
            handle,
            Handle::Request(StagingRequest::Prepare { size: 8 })
        ));
        assert_eq!(service.status().state, State::Preparing);
        assert_eq!(service.pending, Some(Action::Publish(StatusCode::Accepted)));
    }

    mod protocol {
        use super::*;

        const JSON_MANIFEST_8: &[u8] =
            br#"{"id":"7ea690cc8c2cd8ed","size":8,"fnv1a64":9126140903112366317}"#;
        const JSON_MANIFEST_6: &[u8] =
            br#"{"id":"97463f6d0098b84a","size":6,"fnv1a64":10900469685490858058}"#;
        const JSON_MANIFEST_0: &[u8] =
            br#"{"id":"cbf29ce484222325","size":0,"fnv1a64":14695981039346656037}"#;
        const JSON_MANIFEST_MISSING_DIGEST: &[u8] = br#"{"id":"7ea690cc8c2cd8ed","size":8}"#;
        const JSON_MANIFEST_UNKNOWN_FIELD: &[u8] =
            br#"{"id":"7ea690cc8c2cd8ed","size":8,"fnv1a64":9126140903112366317,"hash":"sha256"}"#;

        #[test]
        fn manifest_rejects_if_one_aligned_chunk_cannot_fit() {
            let mut service = service(64);
            assert!(matches!(
                manifest(&mut service, JSON_MANIFEST_8),
                Handle::Consumed
            ));
            assert_eq!(service.status().state, State::Idle);
            assert_eq!(service.status().code, StatusCode::Idle);
            assert_eq!(service.pending, Some(Action::Publish(StatusCode::Mtu)));
        }

        #[test]
        fn manifest_accepts_exact_qos1_chunk_packet_budget() {
            let required = required_rx_bytes("devices/example/staging/chunk", ID, SLOT);
            assert!(matches!(
                manifest(&mut service(required), JSON_MANIFEST_8),
                Handle::Request(StagingRequest::Prepare { .. })
            ));

            let mut undersized = service(required - 1);
            assert!(matches!(
                manifest(&mut undersized, JSON_MANIFEST_8),
                Handle::Consumed
            ));
            assert_eq!(undersized.pending, Some(Action::Publish(StatusCode::Mtu)));
        }

        #[test]
        fn manifest_rejects_invalid_json_profiles() {
            for payload in [
                JSON_MANIFEST_0,
                JSON_MANIFEST_MISSING_DIGEST,
                JSON_MANIFEST_UNKNOWN_FIELD,
                br#"{"id":"","size":8,"fnv1a64":9126140903112366317}"#,
                br#"{"id":"bad/id","size":8,"fnv1a64":9126140903112366317}"#,
                br#"{"id":"7ea690cc8c2cd8ed","size":8,"fnv1a64":9126140903112366317}x"#,
            ] {
                let mut service = service(128);
                assert!(matches!(manifest(&mut service, payload), Handle::Consumed));
                assert_eq!(service.status().state, State::Idle);
                assert_eq!(service.pending, Some(Action::Publish(StatusCode::Error)));
            }
        }

        #[test]
        fn oversized_manifest_is_one_shot_and_next_manifest_can_succeed() {
            let mut service = service(128);
            let mut backend = Backend::default();
            let oversized =
                br#"{"id":"7ea690cc8c2cd8ed","size":129,"fnv1a64":9126140903112366317}"#;

            assert!(matches!(
                manifest(&mut service, oversized),
                Handle::Consumed
            ));
            assert_eq!(service.status().state, State::Idle);
            assert_eq!(service.pending, Some(Action::Publish(StatusCode::Oversize)));

            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            assert_eq!(service.status().state, State::Ready);
            assert_eq!(backend.prepared, Some(8));
        }

        #[test]
        fn ordered_chunks_write_immediately_and_finish() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            assert_eq!(service.state, State::Ready);

            drive_chunk(&mut service, &mut backend, ID, "0", &[1, 2, 3, 4]);
            assert_eq!(service.state, State::Ready);

            drive_chunk(&mut service, &mut backend, ID, "4", &[5, 6, 7, 8]);
            assert_eq!(service.state, State::Complete);

            assert_eq!(backend.prepared, Some(8));
            assert_eq!(
                backend.writes,
                std::vec![(0, std::vec![1, 2, 3, 4]), (4, std::vec![5, 6, 7, 8])]
            );
            assert_eq!(backend.finished, Some(8));
        }

        #[test]
        fn duplicate_final_chunk_replays_complete_status() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            drive_chunk(&mut service, &mut backend, ID, "0", &[1, 2, 3, 4]);
            drive_chunk(&mut service, &mut backend, ID, "4", &[5, 6, 7, 8]);
            service.pending = None;

            assert!(matches!(
                chunk(&mut service, ID, "4", &[5, 6, 7, 8]),
                Handle::Consumed
            ));
            assert_eq!(service.state, State::Complete);
            assert_eq!(service.pending, Some(Action::Publish(StatusCode::Complete)));
            assert_eq!(backend.writes.len(), 2);
        }

        #[test]
        fn unrelated_chunks_do_not_disrupt_active_transfer() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            service.pending = None;

            assert!(matches!(
                service.handle_publish_with_properties(
                    service.chunk_topic().as_str(),
                    &[1, 2, 3, 4],
                    &Properties::from_slice(&[]),
                ),
                Handle::Consumed
            ));
            assert_eq!(service.state, State::Ready);
            assert_eq!(service.pending, None);

            assert!(matches!(
                chunk(&mut service, b"another-transfer", "0", &[1, 2, 3, 4]),
                Handle::Consumed
            ));
            assert_eq!(service.state, State::Ready);
            assert_eq!(service.pending, None);
        }

        #[test]
        fn future_offset_is_rejected_without_write() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            assert!(matches!(
                chunk(&mut service, ID, "4", &[5, 6, 7, 8]),
                Handle::Consumed
            ));

            assert!(backend.writes.is_empty());
            assert_eq!(backend.finished, None);
        }

        #[test]
        fn chunks_wait_for_the_pending_storage_write() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            service.pending = None;

            assert!(matches!(
                chunk(&mut service, ID, "0", &[1, 2, 3, 4]),
                Handle::Request(StagingRequest::Write(_))
            ));
            assert!(matches!(
                chunk(&mut service, ID, "0", &[1, 2, 3, 4]),
                Handle::Consumed
            ));
            assert!(matches!(
                chunk(&mut service, ID, "4", &[5, 6, 7, 8]),
                Handle::Consumed
            ));
            assert_eq!(service.state, State::Writing);
            assert_eq!(service.pending, None);

            service.complete_request(true);
            assert_eq!(service.state, State::Ready);
            assert_eq!(service.status().next_offset, 4);
        }

        #[test]
        fn overlapping_manifest_does_not_disrupt_active_transfer() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            service.pending = None;
            assert!(matches!(
                manifest(&mut service, JSON_MANIFEST_6),
                Handle::Consumed
            ));

            assert_eq!(backend.prepared, Some(8));
            assert_eq!(service.state, State::Ready);
            assert_eq!(service.pending, None);
            let transfer = service.transfer.unwrap();
            assert_eq!(transfer.size, 8);
            assert_eq!(transfer.next_offset, 0);
        }

        #[test]
        fn duplicate_manifest_replays_active_status() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            service.pending = None;

            assert!(matches!(
                manifest(&mut service, JSON_MANIFEST_8),
                Handle::Consumed
            ));
            assert_eq!(service.state, State::Ready);
            assert_eq!(service.pending, Some(Action::Publish(StatusCode::Accepted)));
            assert_eq!(backend.prepared, Some(8));
        }

        #[test]
        fn final_chunk_may_be_erased_value_padded() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_6);
            drive_chunk(&mut service, &mut backend, ID_6, "0", &[1, 2, 3, 4]);
            drive_chunk(&mut service, &mut backend, ID_6, "4", &[5, 6, 0xff, 0xff]);
            assert_eq!(service.state, State::Complete);

            assert_eq!(
                backend.writes,
                std::vec![(0, std::vec![1, 2, 3, 4]), (4, std::vec![5, 6, 0xff, 0xff])]
            );
            assert_eq!(backend.finished, Some(6));
        }

        #[test]
        fn final_chunk_rejects_non_erased_padding() {
            let mut service = service(128);
            let mut backend = Backend::default();
            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_6);
            drive_chunk(&mut service, &mut backend, ID_6, "0", &[1, 2, 3, 4]);
            assert!(matches!(
                chunk(&mut service, ID_6, "4", &[5, 6, 0, 0]),
                Handle::Consumed
            ));

            assert_eq!(backend.finished, None);
        }

        #[test]
        fn backend_failure_marks_transfer_error() {
            let mut service = service(128);
            let mut backend = Backend {
                fail_finish: true,
                ..Backend::default()
            };

            drive_manifest(&mut service, &mut backend, JSON_MANIFEST_8);
            drive_chunk(&mut service, &mut backend, ID, "0", &[1, 2, 3, 4]);
            drive_chunk(&mut service, &mut backend, ID, "4", &[5, 6, 7, 8]);
            assert_eq!(service.state, State::Error);
            assert_eq!(service.pending, Some(Action::Publish(StatusCode::Storage)));
        }
    }
}
