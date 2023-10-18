//! Module for working with "messages".
//!
//! A message is an arbitrary bag of bytes sent by a node to a different node. A message is
//! considered application defined data (L7), and we make no assumptions of any kind regarding the
//! structure. We only care about sending the message to the remote in the most reliable way
//! possible.

use std::{
    collections::HashMap,
    marker::PhantomData,
    net::IpAddr,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
    time::{self, Duration},
};

use futures::{Stream, StreamExt};
use log::{debug, error, warn};
use rand::Fill;

use crate::{
    crypto::PacketBuffer,
    data::DataPlane,
    message::{chunk::MessageChunk, done::MessageDone, init::MessageInit},
};

mod chunk;
mod done;
mod init;

/// The average size of a single chunk. This is mainly intended to preallocate the chunk array on
/// the receiver size. This value should allow reasonable overhead for standard MTU.
const AVERAGE_CHUNK_SIZE: usize = 1_300;
/// The minimum size of a data chunk. Chunks which have a size smaller than this are rejected. An
/// exception is made for the last chunk.
const MINIMUM_CHUNK_SIZE: u64 = 250;

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

/// Length of a message checksum in bytes.
const MESSAGE_CHECKSUM_LENGTH: usize = 32;

pub type Checksum = [u8; MESSAGE_CHECKSUM_LENGTH];

#[derive(Clone)]
pub struct MessageStack {
    // The DataPlane is wrappen in a Mutex since it does not implement Sync.
    data_plane: Arc<Mutex<DataPlane>>,
    inbox: Arc<Mutex<MessageInbox>>,
    outbox: Arc<Mutex<MessageOutbox>>,
}

struct MessageOutbox {
    msges: HashMap<MessageId, OutboundMessageInfo>,
}

struct MessageInbox {
    /// Messages which are still being transmitted.
    // TODO: MessageID is part of ReceivedMessageInfo, rework this into HashSet?
    pending_msges: HashMap<MessageId, ReceivedMessageInfo>,
    /// Messages which have been completed.
    // TODO: MessageID is part of Message, rework this into HashSet?
    complete_msges: HashMap<MessageId, Message>,
}

struct ReceivedMessageInfo {
    id: MessageId,
    src: IpAddr,
    dst: IpAddr,
    /// Length of the finished message.
    len: u64,
    chunks: Vec<Option<Chunk>>,
}

/// A chunk of a message. This represents individual data pieces on the receiver side.
#[derive(Clone)]
struct Chunk {
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

#[derive(PartialEq)]
enum TransmissionState {
    /// Transmission has not started yet.
    Init,
    /// Transmission is in progress (ACK received for INIT).
    InProgress,
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
    /// Create a new `MessageStack`. This uses the provided [`DataPlane`] to inject message
    /// packets. Received packets must be injected into the `MessageStack` through the provided
    /// [`Stream`].
    pub fn new<S>(data_plane: DataPlane, message_packet_stream: S) -> Self
    where
        S: Stream<Item = Result<(PacketBuffer, IpAddr, IpAddr), std::io::Error>>
            + Send
            + Unpin
            + 'static,
    {
        let ms = Self {
            data_plane: Arc::new(Mutex::new(data_plane)),
            inbox: Arc::new(Mutex::new(MessageInbox::new())),
            outbox: Arc::new(Mutex::new(MessageOutbox::new())),
        };

        tokio::task::spawn(
            ms.clone()
                .handle_incoming_message_packets(message_packet_stream),
        );

        ms
    }

    /// Handle incoming messages from the [`DataPlane`].
    async fn handle_incoming_message_packets<S>(self, mut message_packet_stream: S)
    where
        S: Stream<Item = Result<(PacketBuffer, IpAddr, IpAddr), std::io::Error>>
            + Send
            + Unpin
            + 'static,
    {
        while let Some(maybe_message) = message_packet_stream.next().await {
            let (packet, src, dst) = match maybe_message {
                Ok((packet, src, dst)) => (packet, src, dst),
                Err(e) => {
                    error!("Error reading message packet: {e}");
                    // TODO: is this fatal?
                    continue;
                }
            };

            let mp = MessagePacket::new(packet);

            if mp.header().flags().ack() {
                self.handle_message_reply(mp);
            } else {
                self.handle_message(mp, src, dst);
            }
        }

        warn!("Incoming message packet stream ended!");
    }

    /// Handle an incoming message packet which is a reply to a message we previously sent.
    fn handle_message_reply(&self, mp: MessagePacket) {
        let header = mp.header();
        let message_id = header.message_id();
        let flags = header.flags();
        if flags.init() {
            let mut outbox = self.outbox.lock().unwrap();
            if let Some(message) = outbox.msges.get_mut(&message_id) {
                if message.state != TransmissionState::Init {
                    debug!("Dropping INIT ACK for message not in init state");
                    return;
                }
                message.state = TransmissionState::InProgress;
                // Transform message into chunks.
                let mut chunks =
                    Vec::with_capacity((message.len + AVERAGE_CHUNK_SIZE - 1) / AVERAGE_CHUNK_SIZE);
                for (chunk_idx, data_chunk) in
                    message.msg.data.chunks(AVERAGE_CHUNK_SIZE).enumerate()
                {
                    chunks.push(ChunkState {
                        chunk_idx,
                        chunk_offset: chunk_idx * AVERAGE_CHUNK_SIZE,
                        chunk_size: data_chunk.len(),
                        chunk_transmit_state: ChunkTransmitState::Started,
                    })
                }
                message.chunks = chunks;
            }
        } else if flags.chunk() {
            // ACK for a chunk, mark chunk as received so it is not retried again.
            let mut outbox = self.outbox.lock().unwrap();
            if let Some(message) = outbox.msges.get_mut(&message_id) {
                if message.state != TransmissionState::InProgress {
                    debug!("Dropping CHUNK ACK for message not being transmitted");
                    return;
                }
                let mc = MessageChunk::new(mp);
                // Sanity checks. This is just to protect ourselves, if the other party is
                // malicious it can return any data it wants here.
                if mc.chunk_idx() > message.chunks.len() as u64 {
                    debug!("Dropping CHUNK ACK for message because ACK'ed chunk is out of bounds");
                    return;
                }
                // Don't check data size. It is the repsonsiblity of the other party to ensure he
                // ACKs the right chunk. Additionally a malicious node could return a crafted input
                // here anyway.

                message.chunks[mc.chunk_idx() as usize].chunk_transmit_state =
                    ChunkTransmitState::Acked;
            }
        } else if flags.done() {
            // ACK for full message.
            let mut outbox = self.outbox.lock().unwrap();
            if let Some(message) = outbox.msges.get_mut(&message_id) {
                if message.state != TransmissionState::InProgress {
                    debug!("Dropping DONE ACK for message which is not being transmitted");
                    return;
                }
                message.state = TransmissionState::Received;
            }
        } else if flags.read() {
            // Ack for a read flag. Since the original read flag is sent by the receiver, this
            // means the sender indicates he has successfully received the notification that a
            // userspace process has read the message. Note that read flags are only sent once, and
            // this ack is only sent at most once, even if it gets lost. As a result, there is
            // nothing to really do here, and this behavior (ACK READ) might be dropped in the
            // future.
            debug!("Received READ ACK");
        } else {
            debug!("Received unknown ACK message flags {:x}", flags.flags);
        }
    }

    /// Handle an incoming message packet which is **not** a reply to a packet we previously sent.
    fn handle_message(&self, mp: MessagePacket, src: IpAddr, dst: IpAddr) {
        let header = mp.header();
        let message_id = header.message_id();
        let flags = header.flags();
        let reply = if flags.init() {
            // We receive a new message with an ID. If we already have a complete message, ignore
            // it.
            let mut inbox = self.inbox.lock().unwrap();
            if inbox.complete_msges.contains_key(&message_id) {
                debug!("Dropping INIT message as we already have a complete message with this ID");
                return;
            }
            // Otherwise unilaterally reset the state. The message id space is large enough to
            // avoid accidental collisions.
            let mi = MessageInit::new(mp);
            let expected_chunks =
                (mi.length() as usize + AVERAGE_CHUNK_SIZE - 1) / AVERAGE_CHUNK_SIZE;
            let chunks = vec![None; expected_chunks];
            let message = ReceivedMessageInfo {
                id: message_id,
                src,
                dst,
                len: mi.length(),
                chunks,
            };

            if inbox.pending_msges.insert(message_id, message).is_some() {
                debug!("Dropped current pending message because we received a new message with INIT flag set for the same ID");
            }

            Some(mi.into_reply().into_inner())
        } else if flags.chunk() {
            // A chunk can only be received for incomplete messages. We don't have to check the
            // completed messages. Either there is none, so no problem, or there is one, in which
            // case we consider this to be a lingering chunk which was already accepted in the
            // meantime (as the message is complete).
            //
            // SAFETY: a malcious node could send a lot of empty chunks, which trigger allocations
            // to hold the chunk array, effectively exhausting memory. As such, we first need to
            // determine if the chunk is feasible.
            let mut inbox = self.inbox.lock().unwrap();
            if let Some(message) = inbox.pending_msges.get_mut(&message_id) {
                let mc = MessageChunk::new(mp);
                // Make sure the data is within bounds of the message being sent.
                if message.len < mc.chunk_offset() + mc.chunk_size() {
                    debug!("Dropping invalid message CHUNK for being out of bounds");
                    return;
                }
                // Check max chunk idx.
                let max_chunk_idx = (message.len + MINIMUM_CHUNK_SIZE - 1) / MINIMUM_CHUNK_SIZE;
                if mc.chunk_idx() > max_chunk_idx {
                    debug!("Dropping CHUNK because index is too high");
                    return;
                }
                // Check chunk size, allow exception on last chunk.
                if mc.chunk_size() < MINIMUM_CHUNK_SIZE && mc.chunk_idx() != max_chunk_idx {
                    debug!("Dropping CHUNK which is too small");
                    return;
                }
                // Finally check if we have sufficient space for our chunks.
                if message.chunks.len() as u64 <= mc.chunk_idx() {
                    // TODO: optimize
                    let chunks =
                        vec![None; (mc.chunk_idx() + 1 - message.chunks.len() as u64) as usize];
                    message.chunks.extend_from_slice(&chunks);
                }
                // Now insert the chunk. Overwrite any previous chunk.
                message.chunks[mc.chunk_idx() as usize] = Some(Chunk {
                    data: mc.data().to_vec(),
                });

                Some(mc.into_reply().into_inner())
            } else {
                None
            }
        } else if flags.done() {
            let mut inbox = self.inbox.lock().unwrap();
            let md = MessageDone::new(mp);
            // At this point, we should have all message chunks. Verify length and reassemble them.
            if let Some(inbound_message) = inbox.pending_msges.get_mut(&message_id) {
                // Track total size of data we have allocated.
                let mut chunk_size = 0;
                let mut message_data = Vec::with_capacity(inbound_message.len as usize);

                // Chunks are inserted in order.
                for chunk in &inbound_message.chunks {
                    if let Some(chunk) = chunk {
                        message_data.extend_from_slice(&chunk.data);
                        chunk_size += chunk.data.len();
                    } else {
                        // A none chunk is not possible, we should have all chunks
                        debug!("DONE received for incomplete message");
                        return;
                    }
                }

                // TODO: report back here if there is an error.
                if chunk_size as u64 != inbound_message.len {
                    debug!("Message has invalid size");
                    return;
                }

                let message = Message {
                    id: inbound_message.id,
                    src: inbound_message.src,
                    dst: inbound_message.dst,
                    data: message_data,
                };

                let checksum = message.checksum();

                if checksum != md.checksum() {
                    debug!(
                        "Message has wrong checksum, got {} expected {}",
                        md.checksum().to_hex(),
                        checksum.to_hex()
                    );
                    return;
                }

                // Move message to be read.
                inbox.complete_msges.insert(message_id, message);
                inbox.pending_msges.remove(&message_id);

                Some(md.into_reply().into_inner())
            } else {
                None
            }
        } else if flags.read() {
            let mut outbox = self.outbox.lock().unwrap();
            if let Some(message) = outbox.msges.get_mut(&message_id) {
                if message.state != TransmissionState::Received {
                    debug!("Got READ for message which is not in received state");
                    return;
                }
                message.state = TransmissionState::Read;
            }
            None
        } else if flags.aborted() {
            // If the message is not finished yet, discard it completely.
            // But if it is finished, ignore this, i.e, nothing to do.
            let mut inbox = self.inbox.lock().unwrap();
            if inbox.pending_msges.remove(&message_id).is_some() {
                debug!("Dropping pending message because we received an ABORT");
            }
            None
        } else {
            debug!("Received unknown message flags {:x}", flags.flags);
            None
        };
        if let Some(reply) = reply {
            // This is a reply, so SRC -> DST and DST -> SRC
            // FIXME: this can be fixed once the dataplane accepts generic IpAddr addresses.
            match (src, dst) {
                (IpAddr::V6(src), IpAddr::V6(dst)) => {
                    self.data_plane.lock().unwrap().inject_message_packet(
                        dst,
                        src,
                        reply.into_inner(),
                    );
                }
                _ => debug!("can only reply to message fragments if both src and dst are IPv6"),
            }
        }
    }
}

impl MessageStack {
    /// Push a new message to be transmitted, which will be tried for the given duration.
    pub fn push_message(&self, dst: IpAddr, data: Vec<u8>, try_duration: Duration) -> MessageId {
        let src = self
            .data_plane
            .lock()
            .unwrap()
            .router()
            .node_public_key()
            .address()
            .into();

        let id = MessageId::new();

        let len = data.len();
        let msg = Message { id, src, dst, data };

        let created = std::time::SystemTime::now();
        let deadline = created + try_duration;

        let obmi = OutboundMessageInfo {
            state: TransmissionState::Init,
            created,
            deadline,
            len,
            msg,
            chunks: vec![], // leave Vec empty at start
        };

        self.outbox
            .lock()
            .expect("Outbox lock isn't poisoned; qed")
            .insert(obmi);

        // Already send the init packet.
        let mut mp = MessagePacket::new(PacketBuffer::new());
        mp.header_mut().set_message_id(id);

        let mut mi = MessageInit::new(mp);
        mi.set_length(len as u64);
        match (src, dst) {
            (IpAddr::V6(src), IpAddr::V6(dst)) => {
                self.data_plane.lock().unwrap().inject_message_packet(
                    src,
                    dst,
                    mi.into_inner().into_inner(),
                );
            }
            _ => debug!("Can only send messages between two IPv6 addresses"),
        }

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

/// An owned [`PacketBuffer`] for working with messages.
pub struct MessagePacket {
    packet: PacketBuffer,
}

impl MessagePacket {
    /// Create a new `MessagePacket` in the given [`PacketBuffer`].
    pub fn new(packet: PacketBuffer) -> Self {
        Self { packet }
    }

    /// Get a read only reference to the usable space in the buffer.
    pub fn buffer(&self) -> &[u8] {
        &self.packet.buffer()[MESSAGE_HEADER_SIZE..]
    }

    /// Get a mutable reference to the usable space in the buffer.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        &mut self.packet.buffer_mut()[MESSAGE_HEADER_SIZE..]
    }

    /// Get a read only reference to the header of the `MessagePacket`.
    pub fn header(&self) -> MessagePacketHeader {
        MessagePacketHeader {
            header: self.packet.buffer()[..MESSAGE_HEADER_SIZE]
                .try_into()
                .expect("Packet contains enough data for a header; qed"),
        }
    }

    /// Get a mutable reference to the header of the `MessagePacket`.
    pub fn header_mut(&mut self) -> MessagePacketHeaderMut {
        MessagePacketHeaderMut {
            header: <&mut [u8] as TryInto<&mut [u8; MESSAGE_HEADER_SIZE]>>::try_into(
                &mut self.packet.buffer_mut()[..MESSAGE_HEADER_SIZE],
            )
            .expect("Packet contains enough data for a header; qed"),
        }
    }

    /// Consumes this `MessagePacket`, returning the underlying [`PacketBuffer`].
    pub fn into_inner(self) -> PacketBuffer {
        self.packet
    }
}

/// A MessagePacketMut exposes the mutable useable body of a `PacketBuffer` for working with messages.
struct MessagePacketMut<'a> {
    packet: &'a mut PacketBuffer,
}

impl<'a> From<&'a mut PacketBuffer> for MessagePacketMut<'a> {
    fn from(packet: &'a mut PacketBuffer) -> Self {
        MessagePacketMut { packet }
    }
}

impl<'a> MessagePacketMut<'a> {
    /// Get a mutable reference to the header in this message packet.
    fn header_mut(&mut self) -> MessagePacketHeaderMut {
        MessagePacketHeaderMut {
            header: <&mut [u8] as TryInto<&mut [u8; MESSAGE_HEADER_SIZE]>>::try_into(
                &mut self.packet.buffer_mut()[..MESSAGE_HEADER_SIZE],
            )
            .expect("Buffer is big enough for a header; qed"),
        }
    }

    /// Get a mutable reference to the message packet body.
    fn buffer_mut(&mut self) -> &mut [u8] {
        &mut self.packet.buffer_mut()[MESSAGE_HEADER_SIZE..]
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

// A reference to the flags in a message header.
struct Flags<'a> {
    flags: u16,
    // We explicitly tie the reference used to create the Flags struct to the lifetime of this
    // struct to prevent modification of flags while we have a Flags struct.
    _marker: PhantomData<&'a MessagePacketHeader<'a>>,
}

impl<'a> Flags<'a> {
    /// Check if the MESSAGE_INIT flag is set on the header.
    fn init(&self) -> bool {
        self.flags & FLAG_MESSAGE_INIT != 0
    }

    /// Check if the MESSAGE_DONE flag is set on the header.
    fn done(&self) -> bool {
        self.flags & FLAG_MESSAGE_DONE != 0
    }

    /// Check if the MESSAGE_ABORTED flag is set on the header.
    fn aborted(&self) -> bool {
        self.flags & FLAG_MESSAGE_ABORTED != 0
    }

    /// Check if the MESSAGE_CHUNK flag is set on the header.
    fn chunk(&self) -> bool {
        self.flags & FLAG_MESSAGE_CHUNK != 0
    }

    /// Check if the MESSAGE_READ flag is set on the header.
    fn read(&self) -> bool {
        self.flags & FLAG_MESSAGE_READ != 0
    }

    /// Check if the MESSAGE_ACK flag is set on the header.
    fn ack(&self) -> bool {
        self.flags & FLAG_MESSAGE_ACK != 0
    }
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

impl FlagsMut<'_, '_> {
    /// Sets the MESSAGE_INIT flag on the header.
    fn set_init(&mut self) {
        self.flags |= FLAG_MESSAGE_INIT;
    }

    /// Sets the MESSAGE_DONE flag on the header.
    fn set_done(&mut self) {
        self.flags |= FLAG_MESSAGE_DONE;
    }

    /// Sets the MESSAGE_ABORTED flag on the header.
    fn set_aborted(&mut self) {
        self.flags |= FLAG_MESSAGE_ABORTED;
    }

    /// Sets the MESSAGE_CHUNK flag on the header.
    fn set_chunk(&mut self) {
        self.flags |= FLAG_MESSAGE_CHUNK;
    }

    /// Sets the MESSAGE_READ flag on the header.
    fn set_read(&mut self) {
        self.flags |= FLAG_MESSAGE_READ;
    }

    /// Sets the MESSAGE_ACK flag on the header.
    fn set_ack(&mut self) {
        self.flags |= FLAG_MESSAGE_ACK;
    }
}

// Header layout:
//   - 8 bytes message id
//   - 2 bytes flags
//   - 2 bytes reserved
impl<'a> MessagePacketHeader<'a> {
    /// Get the [`MessageId`] from the buffer.
    fn message_id(&self) -> MessageId {
        MessageId(
            self.header[..MESSAGE_ID_SIZE]
                .try_into()
                .expect("Buffer is properly sized; qed"),
        )
    }

    /// Get a reference to the [`Flags`] in this header.
    fn flags(&self) -> Flags<'_> {
        let flags = u16::from_be_bytes(
            self.header[MESSAGE_ID_SIZE..MESSAGE_ID_SIZE + 2]
                .try_into()
                .expect("Slice has a length of 2 which is valid for a u16; qed"),
        );
        Flags {
            flags,
            _marker: PhantomData,
        }
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

    /// Get a reference to the [`Flags`] in this header.
    fn flags(&self) -> Flags<'_> {
        let flags = u16::from_be_bytes(
            self.header[MESSAGE_ID_SIZE..MESSAGE_ID_SIZE + 2]
                .try_into()
                .expect("Slice has a length of 2 which is valid for a u16; qed"),
        );
        Flags {
            flags,
            _marker: PhantomData,
        }
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
    src: IpAddr,
    /// Destination IP
    dst: IpAddr,
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
    /// Length of the message.
    len: usize,
    /// The message to send.
    msg: Message,
    /// Chunks of the message.
    chunks: Vec<ChunkState>,
}

/// A message checksum. In practice this is a 32 byte blake3 digest of the entire message.
pub type MessageChecksum = blake3::Hash;

impl Message {
    /// Calculates the [`MessageChechsum`] of the message.
    ///
    /// Currently this is a 32 byte blake3 hash.
    pub fn checksum(&self) -> MessageChecksum {
        blake3::hash(&self.data)
    }
}

#[cfg(test)]
mod tests {

    use super::{MessagePacketHeaderMut, MESSAGE_HEADER_SIZE};

    #[test]
    fn set_init_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.flags_mut().set_init();

        assert!(buf_mut.flags().init());
        assert_eq!(buf_mut.header[8], 0b1000_0000);
    }

    #[test]
    fn set_done_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.flags_mut().set_done();

        assert!(buf_mut.flags().done());
        assert_eq!(buf_mut.header[8], 0b0100_0000);
    }

    #[test]
    fn set_aborted_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.flags_mut().set_aborted();

        assert!(buf_mut.flags().aborted());
        assert_eq!(buf_mut.header[8], 0b0010_0000);
    }

    #[test]
    fn set_chunk_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.flags_mut().set_chunk();

        assert!(buf_mut.flags().chunk());
        assert_eq!(buf_mut.header[8], 0b0001_0000);
    }

    #[test]
    fn set_read_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.flags_mut().set_read();

        assert!(buf_mut.flags().read());
        assert_eq!(buf_mut.header[8], 0b0000_1000);
    }

    #[test]
    fn set_ack_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.flags_mut().set_ack();

        assert!(buf_mut.flags().ack());
        assert_eq!(buf_mut.header[8], 0b0000_0001);
    }

    #[test]
    fn set_mutli_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.flags_mut().set_init();
        buf_mut.flags_mut().set_ack();

        assert!(buf_mut.flags().ack() && buf_mut.flags().init());
        assert_eq!(buf_mut.header[8], 0b1000_0001);
    }
}
