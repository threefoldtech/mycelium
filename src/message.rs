//! Module for working with "messages".
//!
//! A message is an arbitrary bag of bytes sent by a node to a different node. A message is
//! considered application defined data (L7), and we make no assumptions of any kind regarding the
//! structure. We only care about sending the message to the remote in the most reliable way
//! possible.

use std::{
    collections::HashMap,
    net::Ipv6Addr,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
    time::{self, Duration},
};

use rand::Fill;

use crate::data::DataPlane;

/// The size in bytes of the message header which starts each user message packet.
const MESSAGE_HEADER_SIZE: usize = 12;
/// The size in bytes of a message ID.
const MESSAGE_ID_SIZE: usize = 8;

/// Flag indicating we are starting a new message. The message ID is specified in the header. The
/// body contains the length of the message. The receiver must create an entry for the new ID. This
/// flag must always be set on the first packet of a message stream. If a receiver already received
/// data for this message and a new packet comes in with this flag set for this message, all
/// existing data must be removed on the receiver side.
const FLAG_MESSAGE_INIT: u16 = 0b1000_0000_0000_0000;
// Flag indicating the message with the given ID is done, i.e. it has been fully transmitted.
const FLAG_MESSAGE_DONE: u16 = 0b0100_0000_0000_0000;
/// Indicats the message with this ID is aborted by the sender and the receiver should discard it.
/// The receiver can ignore this if it fully received the message.
const FLAG_MESSAGE_ABORTED: u16 = 0b0010_0000_0000_0000;
/// Flag indicating we are transfering a data chunk.
const FLAG_MESSAGE_CHUNK: u16 = 0b0001_0000_0000_0000;
/// Flag indicating the message with the given ID has been read by the receiver, that is it has
/// been transfered to an external process.
const FLAG_MESSAGE_READ: u16 = 0b0000_1000_0000_0000;
/// Flag acknowledging receipt of a packet. Once this has been received, the packet __should not__ be
/// transmitted again by the sender.
const FLAG_MESSAGE_ACK: u16 = 0b0000_0001_0000_0000;

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
pub struct MessageId([u8; MESSAGE_ID_SIZE]);

impl MessageId {
    fn new() -> Self {
        let mut id = Self([0u8; 8]);

        id.0.try_fill(&mut rand::thread_rng())
            .expect("Can instantiate new ID from thread RNG generator; qed");

        id
    }
}

/// A reference to a header in a message packet.
pub struct MessagePacketHeader<'a> {
    header: &'a [u8; MESSAGE_HEADER_SIZE],
}

/// A mutable reference to a header in a message packet.
pub struct MessagePacketHeaderMut<'a> {
    header: &'a mut [u8; MESSAGE_HEADER_SIZE],
}

/// A mutable reference to the flags in a message header.
// We keep a separate struct because creating a u16 from a byte buffer will write the data in
// native endiannes, but we need big endian. So we add a drop implementation which forces a big
// endian writeback to the buffer.
struct FlagsMut<'a, 'b> {
    header: &'b mut MessagePacketHeaderMut<'a>,
    flags: u16,
}

impl Drop for FlagsMut<'_, '_> {
    fn drop(&mut self) {
        // Explicitly write back the flags in big endian format
        self.header[MESSAGE_ID_SIZE..MESSAGE_ID_SIZE + 2].copy_from_slice(&self.flags.to_be_bytes())
    }
}

// Header layout:
//   - 8 bytes message id
//   - 2 bytes flags
//   - 2 bytes reserved
impl<'a> MessagePacketHeaderMut<'a> {
    /// Get the [`MessageId`] from the buffer.
    fn message_id(&self) -> MessageId {
        MessageId(
            self.header[..MESSAGE_ID_SIZE]
                .try_into()
                .expect("Buffer is properly sized; qed"),
        )
    }

    /// Set the [`MessageId`] in the buffer to the provided value.
    fn set_message_id(&mut self, mid: MessageId) {
        self.header[..MESSAGE_ID_SIZE].copy_from_slice(&mid.0[..]);
    }

    /// Get a mutable reference to the flags in this header.
    // Note: we explicitly name lifetimes here, as elliding would give the mutable ref to self
    // lifetime '1, which is not the same as the elided lifetime 'b on the struct, which would then
    // cause the compiler to force the FlagsMut struct to as long as self and not be dropped.
    fn flags_mut<'b>(&'b mut self) -> FlagsMut<'a, 'b> {
        let flags = u16::from_be_bytes(
            self.header[MESSAGE_ID_SIZE..MESSAGE_ID_SIZE + 2]
                .try_into()
                .expect("Slice has a length of 2 which is valid for a u16; qed"),
        );
        FlagsMut {
            header: self,
            flags,
        }
    }

    /// Sets the MESSAGE_INIT flag on the header.
    fn set_init(&mut self) {
        self.flags_mut().flags |= FLAG_MESSAGE_INIT;
    }

    /// Sets the MESSAGE_DONE flag on the header.
    fn set_done(&mut self) {
        self.flags_mut().flags |= FLAG_MESSAGE_DONE;
    }

    /// Sets the MESSAGE_ABORTED flag on the header.
    fn set_aborted(&mut self) {
        self.flags_mut().flags |= FLAG_MESSAGE_ABORTED;
    }

    /// Sets the MESSAGE_CHUNK flag on the header.
    fn set_chunk(&mut self) {
        self.flags_mut().flags |= FLAG_MESSAGE_CHUNK;
    }

    /// Sets the MESSAGE_READ flag on the header.
    fn set_read(&mut self) {
        self.flags_mut().flags |= FLAG_MESSAGE_READ;
    }

    /// Sets the MESSAGE_ACK flag on the header.
    fn set_ack(&mut self) {
        self.flags_mut().flags |= FLAG_MESSAGE_ACK;
    }
}

impl<'a> Deref for MessagePacketHeader<'a> {
    type Target = [u8; MESSAGE_HEADER_SIZE];

    fn deref(&self) -> &Self::Target {
        self.header
    }
}

impl<'a> Deref for MessagePacketHeaderMut<'a> {
    type Target = [u8; MESSAGE_HEADER_SIZE];

    fn deref(&self) -> &Self::Target {
        self.header
    }
}

impl<'a> DerefMut for MessagePacketHeaderMut<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.header
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

#[cfg(test)]
mod tests {

    use super::{MessagePacketHeaderMut, MESSAGE_HEADER_SIZE};

    #[test]
    fn set_init_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.set_init();

        assert_eq!(buf_mut.header[8], 0b1000_0000);
    }

    #[test]
    fn set_done_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.set_done();

        assert_eq!(buf_mut.header[8], 0b0100_0000);
    }

    #[test]
    fn set_aborted_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.set_aborted();

        assert_eq!(buf_mut.header[8], 0b0010_0000);
    }

    #[test]
    fn set_chunk_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.set_chunk();

        assert_eq!(buf_mut.header[8], 0b0001_0000);
    }

    #[test]
    fn set_read_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.set_read();

        assert_eq!(buf_mut.header[8], 0b0000_1000);
    }

    #[test]
    fn set_ack_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.set_ack();

        assert_eq!(buf_mut.header[8], 0b0000_0001);
    }

    #[test]
    fn set_mutli_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.set_init();
        buf_mut.set_ack();

        assert_eq!(buf_mut.header[8], 0b1000_0001);
    }
}
