//! Module for working with "messages".
//!
//! A message is an arbitrary bag of bytes sent by a node to a different node. A message is
//! considered application defined data (L7), and we make no assumptions of any kind regarding the
//! structure. We only care about sending the message to the remote in the most reliable way
//! possible.

use std::{
    collections::HashMap,
    net::Ipv6Addr,
    sync::{Arc, Mutex},
    time::{self, Duration},
};

use rand::Fill;

use crate::data::DataPlane;

/// The size in bytes of the message header which starts each user message packet.
const MESSAGE_HEADER_SIZE: usize = 12;

/// Flag indicating we are starting a new message. The message ID is specified in the header. The
/// body contains the length of the message. The receiver must create an entry for the new ID. This
/// flag must always be set on the first packet of a message stream. If a receiver already received
/// data for this message and a new packet comes in with this flag set for this message, all
/// existing data must be removed on the receiver side.
const FLAG_MESSAGE_INIT: u8 = 0b1000_0000;
// Flag indicating the message with the given ID is done, i.e. it has been fully transmitted.
const FLAG_MESSAGE_DONE: u8 = 0b0100_0000;
/// Indicats the message with this ID is aborted by the sender and the receiver should discard it.
/// The receiver can ignore this if it fully received the message.
const FLAG_MESSAGE_ABORTED: u8 = 0b0010_0000;
/// Flag indicating the message with the given ID has been read by the receiver, that is it has
/// been transfered to an external process.
const FLAG_MESSAGE_READ: u8 = 0b0001_0000;
/// Flag acknowledging receipt of a chunk. Once this has been received, the packet __should not__ be
/// transmitted again by the sender.
const FLAG_MESSAGE_CHUNK_ACK: u8 = 0b0000_0001;

pub struct MessageStack {
    data_plane: Arc<DataPlane>,
    inbox: Arc<Mutex<MessageInbox>>,
    outbox: Arc<Mutex<MessageOutbox>>,
}

struct MessageOutbox {
    msges: HashMap<MessageId, OutboundMessageInfo>,
}

struct MessageInbox {
    /// Messages which are still being transmitted.
    pending_msges: HashMap<MessageId, ReceivedMessageInfo>,
    /// Messages which have been completed.
    complete_msges: HashMap<MessageId, Message>,
}

struct ReceivedMessageInfo {
    id: MessageId,
    src: Ipv6Addr,
    dst: Ipv6Addr,
    message: Vec<Chunk>,
}

/// A chunk of a message. This represents individual data pieces on the receiver side.
struct Chunk {
    chunk_idx: usize,
    data: Vec<u8>,
}

/// Description of an individual chunk.
struct ChunkState {
    /// Index of the chunk in the chunk stream.
    chunk_idx: usize,
    /// Offset of the chunk in the message.
    chunk_offset: usize,
    /// Size of the chunk.
    // TODO: is this needed or can this be extrapolated by checking the next chunk in the list?
    chunk_size: usize,
    /// Transmit state of the chunk.
    chunk_transmit_state: ChunkTransmitState,
}

/// Transmission state of an individual chunk
enum ChunkTransmitState {
    /// The chunk hasn't been transmitted yet.
    Started,
    /// The chunk has been sent but we did not receive an acknowledgment yet. The time the chunk
    /// was sent is remembered so we can calulcate if we need to try sending it again.
    Sent(std::time::Instant),
    /// The receiver has acknowledged receipt of the chunk.
    Acked,
}

enum TransmissionState {
    /// Transmission has not started yet.
    Init,
    /// Transmission is in progress.
    InProgress(Vec<ChunkState>),
    /// Remote acknowledged full reception.
    Received,
    /// Remote indicated the message has been read by an external entity.
    Read,
    /// Transmission aborted by us. We indicated this by sending an abort flag to the receiver.
    Aborted,
}

impl MessageInbox {
    fn new() -> Self {
        Self {
            pending_msges: HashMap::new(),
            complete_msges: HashMap::new(),
        }
    }
}

impl MessageOutbox {
    /// Create a new `MessageOutbox` ready for use.
    fn new() -> Self {
        Self {
            msges: HashMap::new(),
        }
    }

    /// Insert a new message for tracking during (and after) sending.
    fn insert(&mut self, msg: OutboundMessageInfo) {
        self.msges.insert(msg.msg.id, msg);
    }
}

impl MessageStack {
    pub fn new(data_plane: DataPlane) -> Arc<Self> {
        Arc::new(Self {
            data_plane: Arc::new(data_plane),
            inbox: Arc::new(Mutex::new(MessageInbox::new())),
            outbox: Arc::new(Mutex::new(MessageOutbox::new())),
        })
    }
}

pub struct MessagePacketHeader {
    data: [u8; MESSAGE_HEADER_SIZE],
}

impl MessageStack {
    /// Push a new message to be transmitted, which will be tried for the given duration.
    pub fn push_message(&self, dst: Ipv6Addr, data: Vec<u8>, try_duration: Duration) -> MessageId {
        let src = self.data_plane.router().node_public_key().address();

        let id = MessageId::new();

        let msg = Message { id, src, dst, data };

        let created = std::time::SystemTime::now();
        let deadline = created + try_duration;

        let obmi = OutboundMessageInfo {
            state: TransmissionState::Init,
            created,
            deadline,
            msg,
        };

        self.outbox
            .lock()
            .expect("Outbox lock isn't poisoned; qed")
            .insert(obmi);

        id
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId([u8; 8]);

impl MessageId {
    fn new() -> Self {
        let mut id = Self([0u8; 8]);

        id.0.try_fill(&mut rand::thread_rng())
            .expect("Can instantiate new ID from thread RNG generator; qed");

        id
    }
}

pub struct Message {
    /// Generated ID, used to identify the message on the wire
    id: MessageId,
    /// Source IP (ours)
    src: Ipv6Addr,
    /// Destination IP
    dst: Ipv6Addr,
    /// Data
    data: Vec<u8>,
}

pub struct OutboundMessageInfo {
    /// The current state of the message
    state: TransmissionState,
    /// Timestamp when the message was created (received by this node).
    created: time::SystemTime,
    /// Timestamp indicating when we stop trying to send the message.
    deadline: time::SystemTime,
    /// The message to send
    msg: Message,
}
