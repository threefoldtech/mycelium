//! Module for working with "messages".
//!
//! A message is an arbitrary bag of bytes sent by a node to a different node. A message is
//! considered application defined data (L7), and we make no assumptions of any kind regarding the
//! structure. We only care about sending the message to the remote in the most reliable way
//! possible.

use core::fmt;
use std::{
    collections::{HashMap, VecDeque},
    marker::PhantomData,
    net::IpAddr,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
    time::{self, Duration},
};

use futures::{Stream, StreamExt};
use rand::Fill;
use serde::{de::Visitor, Deserialize, Deserializer, Serialize};
use tokio::sync::watch;
use tracing::{debug, error, trace, warn};

use crate::{
    crypto::{PacketBuffer, PublicKey},
    data::DataPlane,
    message::{chunk::MessageChunk, done::MessageDone, init::MessageInit},
    metrics::Metrics,
};

mod chunk;
mod done;
mod init;

/// The amount of time to try and send messages before we give up.
const MESSAGE_SEND_WINDOW: Duration = Duration::from_secs(60 * 5);

/// The amount of time to wait before sending a chunk again if receipt is not acknowledged.
const RETRANSMISSION_DELAY: Duration = Duration::from_secs(1);

/// Amount of time between sweeps of the subscriber list to clear orphaned subscribers.
const REPLY_SUBSCRIBER_CLEAR_DELAY: Duration = Duration::from_secs(60);

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
/// Indicates the message with this ID is aborted by the sender and the receiver should discard it.
/// The receiver can ignore this if it fully received the message.
const FLAG_MESSAGE_ABORTED: u16 = 0b0010_0000_0000_0000;
/// Flag indicating we are transferring a data chunk.
const FLAG_MESSAGE_CHUNK: u16 = 0b0001_0000_0000_0000;
/// Flag indicating the message with the given ID has been read by the receiver, that is it has
/// been transferred to an external process.
const FLAG_MESSAGE_READ: u16 = 0b0000_1000_0000_0000;
/// Flag indicating we are sending a reply to a received message. The message ID used is the same
/// as the received message.
const FLAG_MESSAGE_REPLY: u16 = 0b0000_0100_0000_0000;
/// Flag acknowledging receipt of a packet. Once this has been received, the packet __should not__ be
/// transmitted again by the sender.
const FLAG_MESSAGE_ACK: u16 = 0b0000_0001_0000_0000;

/// Length of a message checksum in bytes.
const MESSAGE_CHECKSUM_LENGTH: usize = 32;

/// Checksum of a message used to verify received message integrity.
pub type Checksum = [u8; MESSAGE_CHECKSUM_LENGTH];

/// Response type when pushing a message.
pub type MessagePushResponse = (MessageId, Option<watch::Receiver<Option<ReceivedMessage>>>);

pub struct MessageStack<M> {
    // The DataPlane is wrappen in a Mutex since it does not implement Sync.
    data_plane: Arc<Mutex<DataPlane<M>>>,
    inbox: Arc<Mutex<MessageInbox>>,
    outbox: Arc<Mutex<MessageOutbox>>,
    /// Receiver handle for inbox listeners (basically a condvar).
    subscriber: watch::Receiver<()>,
    /// Subscribers for messages with specific ID's. These are intended to be used when waiting for
    /// a reply.
    /// This takes an Option as value to avoid the hassle of constructing a dummy value when
    /// creating the watch channel.
    reply_subscribers: Arc<Mutex<HashMap<MessageId, watch::Sender<Option<ReceivedMessage>>>>>,
}

struct MessageOutbox {
    msges: HashMap<MessageId, OutboundMessageInfo>,
}

struct MessageInbox {
    /// Messages which are still being transmitted.
    // TODO: MessageID is part of ReceivedMessageInfo, rework this into HashSet?
    pending_msges: HashMap<MessageId, ReceivedMessageInfo>,
    /// Messages which have been completed.
    complete_msges: VecDeque<ReceivedMessage>,
    /// Notification sender used to allert subscribed listeners.
    notify: watch::Sender<()>,
}

struct ReceivedMessageInfo {
    id: MessageId,
    is_reply: bool,
    src: IpAddr,
    dst: IpAddr,
    /// Length of the finished message.
    len: u64,
    /// Optional topic of the message.
    topic: Vec<u8>,
    chunks: Vec<Option<Chunk>>,
}

#[derive(Clone)]
pub struct ReceivedMessage {
    /// Id of the message.
    pub id: MessageId,
    /// This message is a reply to an initial message with the given id.
    pub is_reply: bool,
    /// The overlay ip of the sender.
    pub src_ip: IpAddr,
    /// The public key of the sender of the message.
    pub src_pk: PublicKey,
    /// The overlay ip of the receiver.
    pub dst_ip: IpAddr,
    /// The public key of the receiver of the message. This is always ours.
    pub dst_pk: PublicKey,
    /// The possible topic of the message.
    pub topic: Vec<u8>,
    /// Actual message.
    pub data: Vec<u8>,
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

#[derive(Debug, Clone, Copy)]
pub enum PushMessageError {
    /// The topic set in the message is too large.
    TopicTooLarge,
}

impl MessageInbox {
    fn new(notify: watch::Sender<()>) -> Self {
        Self {
            pending_msges: HashMap::new(),
            complete_msges: VecDeque::new(),
            notify,
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

impl<M> MessageStack<M>
where
    M: Metrics + Clone + Send + 'static,
{
    /// Create a new `MessageStack`. This uses the provided [`DataPlane`] to inject message
    /// packets. Received packets must be injected into the `MessageStack` through the provided
    /// [`Stream`].
    pub fn new<S>(data_plane: DataPlane<M>, message_packet_stream: S) -> Self
    where
        S: Stream<Item = (PacketBuffer, IpAddr, IpAddr)> + Send + Unpin + 'static,
    {
        let (notify, subscriber) = watch::channel(());
        let ms = Self {
            data_plane: Arc::new(Mutex::new(data_plane)),
            inbox: Arc::new(Mutex::new(MessageInbox::new(notify))),
            outbox: Arc::new(Mutex::new(MessageOutbox::new())),
            subscriber,
            reply_subscribers: Arc::new(Mutex::new(HashMap::new())),
        };

        tokio::task::spawn(
            ms.clone()
                .handle_incoming_message_packets(message_packet_stream),
        );

        // task to periodically clear leftover reply subscribers
        {
            let ms = ms.clone();
            tokio::task::spawn(async move {
                loop {
                    tokio::time::sleep(REPLY_SUBSCRIBER_CLEAR_DELAY).await;

                    let mut subs = ms.reply_subscribers.lock().unwrap();
                    subs.retain(|id, v| {
                        if v.receiver_count() == 0 {
                            debug!(
                                "Clearing orphaned subscription for message id {}",
                                id.as_hex()
                            );
                            false
                        } else {
                            true
                        }
                    })
                }
            });
        }
        ms
    }

    /// Handle incoming messages from the [`DataPlane`].
    async fn handle_incoming_message_packets<S>(self, mut message_packet_stream: S)
    where
        S: Stream<Item = (PacketBuffer, IpAddr, IpAddr)> + Send + Unpin + 'static,
    {
        while let Some((packet, src, dst)) = message_packet_stream.next().await {
            let mp = MessagePacket::new(packet);

            trace!(
                "Received message packet with flags {:b}",
                mp.header().flags()
            );

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
                let mut chunks = Vec::with_capacity(message.len.div_ceil(AVERAGE_CHUNK_SIZE));
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
            debug!("Received unknown ACK message flags {:b}", flags);
        }
    }

    /// Handle an incoming message packet which is **not** a reply to a packet we previously sent.
    fn handle_message(&self, mp: MessagePacket, src: IpAddr, dst: IpAddr) {
        let header = mp.header();
        let message_id = header.message_id();
        let flags = header.flags();
        let reply = if flags.init() {
            let is_reply = flags.reply();
            // We receive a new message with an ID. If we already have a complete message, ignore
            // it.
            let mut inbox = self.inbox.lock().unwrap();
            if inbox.complete_msges.iter().any(|m| m.id == message_id) {
                debug!("Dropping INIT message as we already have a complete message with this ID");
                return;
            }
            // Otherwise unilaterally reset the state. The message id space is large enough to
            // avoid accidental collisions.
            let mi = MessageInit::new(mp);
            let expected_chunks = (mi.length() as usize).div_ceil(AVERAGE_CHUNK_SIZE);
            let chunks = vec![None; expected_chunks];
            let message = ReceivedMessageInfo {
                id: message_id,
                is_reply,
                src,
                dst,
                len: mi.length(),
                topic: mi.topic().into(),
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
            // SAFETY: a malicious node could send a lot of empty chunks, which trigger allocations
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
                let max_chunk_idx = message.len.div_ceil(MINIMUM_CHUNK_SIZE);
                if mc.chunk_idx() > max_chunk_idx {
                    debug!("Dropping CHUNK because index is too high");
                    return;
                }
                // Check chunk size, allow exception on last chunk.
                if mc.chunk_size() < MINIMUM_CHUNK_SIZE && mc.chunk_idx() != max_chunk_idx {
                    debug!(
                        "Dropping CHUNK {}/{max_chunk_idx} which is too small ({} bytes / {MINIMUM_CHUNK_SIZE} bytes)",
                        mc.chunk_idx(),
                        mc.chunk_size()
                    );
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
                // Check if we have sufficient chunks
                if md.chunk_count() != inbound_message.chunks.len() as u64 {
                    // TODO: report error to sender
                    debug!("Message has invalid amount of chunks");
                    return;
                }
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
                    topic: inbound_message.topic.clone(),
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

                // Convert the IP's to PublicKeys.
                let dp = self.data_plane.lock().unwrap();
                let src_pubkey = if let Some(pk) = dp.router().get_pubkey(message.src) {
                    pk
                } else {
                    warn!("No public key entry for IP we just received a message chunk from");
                    return;
                };
                // This always is our own key as we are receiving.
                let dst_pubkey = dp.router().node_public_key();

                let message = ReceivedMessage {
                    id: message.id,
                    is_reply: inbound_message.is_reply,
                    src_ip: message.src,
                    src_pk: src_pubkey,
                    dst_ip: message.dst,
                    dst_pk: dst_pubkey,
                    topic: message.topic,
                    data: message.data,
                };

                debug!("Message {} reception complete", message.id.as_hex());

                // Check if we have any listeners and try to send the message to those first.
                let mut subscribers = self.reply_subscribers.lock().unwrap();
                // Use remove here since we are done with the subscriber
                // TODO: only check this if the is_reply flag is set?
                if let Some(sub) = subscribers.remove(&message.id) {
                    if let Err(e) = sub.send(Some(message)) {
                        debug!("Subscriber quit before we could send the reply");
                        // Move message to be read if there were no subscribers.
                        inbox.complete_msges.push_back(e.0.unwrap());
                        // Notify subscribers we have a new message.
                        inbox.notify.send_replace(());
                    } else {
                        debug!("Informed subscriber of message reply");
                    }
                } else {
                    // Move message to be read if there were no subscribers.
                    inbox.complete_msges.push_back(message);
                    // Notify subscribers we have a new message.
                    inbox.notify.send_replace(());
                }
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
                debug!("Receiver confirmed READ of message {}", message_id.as_hex());
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
            debug!("Received unknown message flags {:b}", flags);
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

impl<M> MessageStack<M>
where
    M: Metrics + Clone + Send + 'static,
{
    /// Push a new message to be transmitted, which will be tried for the given duration. A
    /// [message id](MessageId) will be randomly generated, and returned.
    pub fn new_message(
        &self,
        dst: IpAddr,
        data: Vec<u8>,
        topic: Vec<u8>,
        try_duration: Duration,
        subscribe_reply: bool,
    ) -> Result<MessagePushResponse, PushMessageError> {
        self.push_message(None, dst, data, topic, try_duration, subscribe_reply)
    }

    /// Push a new message which is a reply to the message with [the provided id](MessageId).
    pub fn reply_message(
        &self,
        reply_to: MessageId,
        dst: IpAddr,
        data: Vec<u8>,
        try_duration: Duration,
    ) -> MessageId {
        self.push_message(Some(reply_to), dst, data, vec![], try_duration, false)
            .expect("Empty topic is never too large")
            .0
    }

    /// Subscribe to a new message with the given ID. In practice, this will be a reply.
    pub fn subscribe_id(&self, id: MessageId) -> watch::Receiver<Option<ReceivedMessage>> {
        let mut subscribers = self.reply_subscribers.lock().unwrap();
        if let Some(sub) = subscribers.get(&id) {
            sub.subscribe()
        } else {
            // dummy initial value
            let (tx, rx) = watch::channel(None);
            subscribers.insert(id, tx);
            rx
        }
    }

    /// Push a new message. If id is set, it is considered a reply to that id. If not, a new id is
    /// generated.
    fn push_message(
        &self,
        id: Option<MessageId>,
        dst: IpAddr,
        data: Vec<u8>,
        topic: Vec<u8>,
        try_duration: Duration,
        subscribe: bool,
    ) -> Result<MessagePushResponse, PushMessageError> {
        if topic.len() > 255 {
            return Err(PushMessageError::TopicTooLarge);
        }

        let src = self
            .data_plane
            .lock()
            .unwrap()
            .router()
            .node_public_key()
            .address()
            .into();

        let (id, reply) = if let Some(id) = id {
            (id, true)
        } else {
            (MessageId::new(), false)
        };

        let len = data.len();
        let msg = Message {
            id,
            src,
            dst,
            topic,
            data,
        };

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

        let subscription = if subscribe {
            Some(self.subscribe_id(id))
        } else {
            None
        };

        // Already prepare the init packet for sending..
        let mut mp = MessagePacket::new(PacketBuffer::new());
        mp.header_mut().set_message_id(id);
        if reply {
            mp.header_mut().flags_mut().set_reply();
        }

        let mut mi = MessageInit::new(mp);
        mi.set_length(len as u64);
        mi.set_topic(&obmi.msg.topic);

        self.outbox
            .lock()
            .expect("Outbox lock isn't poisoned; qed")
            .insert(obmi);

        // Actually send the init packet
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

        // Clone message stack so it can be injected in the task.
        let message_stack = self.clone();
        tokio::task::spawn(async move {
            let mut deadline = tokio::time::interval(MESSAGE_SEND_WINDOW);
            let mut interval = tokio::time::interval(RETRANSMISSION_DELAY);
            // Avoid a send burst if the system is slow.
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // intervals tick immediately, so consume one tick each
            deadline.tick().await;
            interval.tick().await;

            let mut aborted = false;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if aborted {
                            continue
                        }
                        if let Some(msg) = message_stack.outbox.lock().unwrap().msges.get_mut(&id) {
                            match msg.state {
                                TransmissionState::Init => {
                                    // Send the init packet.
                                    let mut mp = MessagePacket::new(PacketBuffer::new());
                                    mp.header_mut().set_message_id(id);
                                    if reply {
                                        mp.header_mut().flags_mut().set_reply();
                                    }

                                    let mut mi = MessageInit::new(mp);
                                    mi.set_length(len as u64);
                                    mi.set_topic(&msg.msg.topic);
                                    match (msg.msg.src, msg.msg.dst) {
                                        (IpAddr::V6(src), IpAddr::V6(dst)) => {
                                            message_stack
                                                .data_plane
                                                .lock()
                                                .unwrap()
                                                .inject_message_packet(
                                                    src,
                                                    dst,
                                                    mi.into_inner().into_inner(),
                                                );
                                        }
                                        _ => debug!("Can only send messages between two IPv6 addresses"),
                                    }
                                }
                                TransmissionState::InProgress => {
                                    // Send chunks which haven't been sent yet.
                                    let mut all_acked = true;
                                    for chunk in msg.chunks.iter_mut() {
                                        if !matches!(chunk.chunk_transmit_state, ChunkTransmitState::Acked)
                                        {
                                            all_acked = false;
                                        }
                                        match chunk.chunk_transmit_state {
                                            ChunkTransmitState::Started => {
                                                // Generate and send chunk, move chunk to state sent
                                                let mut mp = MessagePacket::new(PacketBuffer::new());
                                                mp.header_mut().set_message_id(id);

                                                let mut mc = MessageChunk::new(mp);
                                                mc.set_chunk_idx(chunk.chunk_idx as u64);
                                                mc.set_chunk_offset(chunk.chunk_offset as u64);
                                                if let Err(e) = mc.set_chunk_data(
                                                    &msg.msg.data[chunk.chunk_offset
                                                        ..chunk.chunk_offset + chunk.chunk_size],
                                                ) {
                                                    error!("Failed to generate and send chunk: {e}");
                                                };

                                                match (msg.msg.src, msg.msg.dst) {
                                                    (IpAddr::V6(src), IpAddr::V6(dst)) => {
                                                        message_stack
                                                            .data_plane
                                                            .lock()
                                                            .unwrap()
                                                            .inject_message_packet(
                                                                src,
                                                                dst,
                                                                mc.into_inner().into_inner(),
                                                            );
                                                    }
                                                    _ => debug!(
                                                        "Can only send messages between two IPv6 addresses"
                                                    ),
                                                }
                                                chunk.chunk_transmit_state =
                                                    ChunkTransmitState::Sent(time::Instant::now());
                                            }
                                            ChunkTransmitState::Sent(t) => {
                                                if t.elapsed().as_secs() >= 1 {
                                                    // retransmit
                                                    let mut mp = MessagePacket::new(PacketBuffer::new());
                                                    mp.header_mut().set_message_id(id);

                                                    let mut mc = MessageChunk::new(mp);
                                                    mc.set_chunk_idx(chunk.chunk_idx as u64);
                                                    mc.set_chunk_offset(chunk.chunk_offset as u64);
                                                    if let Err(e) = mc.set_chunk_data(
                                                        &msg.msg.data[chunk.chunk_offset
                                                            ..chunk.chunk_offset + chunk.chunk_size],
                                                    ) {
                                                        error!("Failed to generate and send chunk: {e}");
                                                    };

                                                    match (msg.msg.src, msg.msg.dst) {
                                                        (IpAddr::V6(src), IpAddr::V6(dst)) => {
                                                            message_stack
                                                                .data_plane
                                                                .lock()
                                                                .unwrap()
                                                                .inject_message_packet(
                                                                    src,
                                                                    dst,
                                                                    mc.into_inner().into_inner(),
                                                                );
                                                        }
                                                        _ => debug!(
                                                        "Can only send messages between two IPv6 addresses"
                                                    ),
                                                    }
                                                    chunk.chunk_transmit_state =
                                                        ChunkTransmitState::Sent(time::Instant::now());
                                                }
                                            }
                                            ChunkTransmitState::Acked => {
                                                // chunk has been acknowledged, nothing to do here.
                                            }
                                        }
                                    }

                                    // If every chunk is acked, send the done packet.
                                    if all_acked {
                                        let mut mp = MessagePacket::new(PacketBuffer::new());
                                        mp.header_mut().set_message_id(id);

                                        let mut md = MessageDone::new(mp);
                                        md.set_chunk_count(msg.chunks.len() as u64);
                                        md.set_checksum(msg.msg.checksum());

                                        match (msg.msg.src, msg.msg.dst) {
                                            (IpAddr::V6(src), IpAddr::V6(dst)) => {
                                                message_stack
                                                    .data_plane
                                                    .lock()
                                                    .unwrap()
                                                    .inject_message_packet(
                                                        src,
                                                        dst,
                                                        md.into_inner().into_inner(),
                                                    );
                                            }
                                            _ => {
                                                debug!("Can only send messages between two IPv6 addresses")
                                            }
                                        };
                                    }
                                }
                                TransmissionState::Received => {
                                    // Nothing to do if the remote acknowledged receipt.
                                }
                                TransmissionState::Read => {
                                    // Nothing to do if the remote acknowledged that the message is read.
                                }
                                TransmissionState::Aborted => {
                                    // Nothing to do if we aborted the message.
                                }
                            };
                        } else {
                            // If the message is gone, just exit
                            return;
                        }
                    },
                    _ = deadline.tick() => {
                        // The first time we get a tick to abort, abort the message if it is not
                        // received yet.
                        // The second time, clean up the storage.
                        if !aborted {
                            aborted = true;
                            if let Some(msg) = message_stack.outbox.lock().unwrap().msges.get_mut(&id) {
                                if matches!(msg.state, TransmissionState::Init | TransmissionState::InProgress) {
                                    msg.state = TransmissionState::Aborted;

                                    // Inform receiver of message abortion.
                                    let mut mp = MessagePacket::new(PacketBuffer::new());
                                    mp.header_mut().set_message_id(id);
                                    mp.header_mut().flags_mut().set_aborted();


                                    match (msg.msg.src, msg.msg.dst) {
                                        (IpAddr::V6(src), IpAddr::V6(dst)) => {
                                            message_stack
                                                .data_plane
                                                .lock()
                                                .unwrap()
                                                .inject_message_packet(
                                                    src,
                                                    dst,
                                                    mp.into_inner(),
                                                );
                                        }
                                        _ => {
                                            debug!("Can only send messages between two IPv6 addresses")
                                        }
                                    };
                                }
                            }
                            continue
                        }

                        // Second tick, clean up.
                        message_stack.outbox.lock().unwrap().msges.remove(&id);
                        return
                    }
                }
            }
        });

        Ok((id, subscription))
    }

    /// Get information about the status of an outbound message.
    pub fn message_info(&self, id: MessageId) -> Option<MessageInfo> {
        let outbox = self.outbox.lock().unwrap();
        outbox.msges.get(&id).map(|mi| MessageInfo {
            dst: mi.msg.dst,
            state: match mi.state {
                TransmissionState::Init => TransmissionProgress::Pending,
                TransmissionState::InProgress => {
                    let (pending, sent, acked) = mi.chunks.iter().fold(
                        (0, 0, 0),
                        |(mut pending, mut sent, mut acked), chunk| {
                            match chunk.chunk_transmit_state {
                                ChunkTransmitState::Started => pending += 1,
                                ChunkTransmitState::Sent(_) => sent += 1,
                                ChunkTransmitState::Acked => acked += 1,
                            };
                            (pending, sent, acked)
                        },
                    );
                    TransmissionProgress::Sending {
                        pending,
                        sent,
                        acked,
                    }
                }
                TransmissionState::Received => TransmissionProgress::Received,
                TransmissionState::Read => TransmissionProgress::Read,
                TransmissionState::Aborted => TransmissionProgress::Aborted,
            },
            created: mi
                .created
                .duration_since(time::UNIX_EPOCH)
                .expect("Message was created after the epoch")
                .as_secs() as i64,
            deadline: mi
                .deadline
                .duration_since(time::UNIX_EPOCH)
                .expect("Message expires after the epoch")
                .as_secs() as i64,
            msg_len: mi.len,
        })
    }

    /// A future which eventually resolves to a new (inbound message)[`ReceivedMessage`], if new messages come in.
    ///
    /// If pop is false, the message is not removed and the next call of this method will return
    /// the same message.
    pub async fn message(&self, pop: bool, topic: Option<Vec<u8>>) -> ReceivedMessage {
        // Copy the subscriber since we need mutable access to it.
        let mut subscriber = self.subscriber.clone();

        loop {
            // Scope to ensure we drop the lock after we checked for a message and don't hold
            // it while waiting for a new notification.
            'check: {
                let mut inbox = self.inbox.lock().unwrap();
                // If a filter is set only check for those messages.
                if let Some(ref topic) = topic {
                    if let Some((idx, _)) = inbox
                        .complete_msges
                        .iter()
                        .enumerate()
                        .find(|(_, v)| &v.topic == topic)
                    {
                        return inbox.complete_msges.remove(idx).unwrap();
                    } else {
                        break 'check;
                    }
                }
                if let Some(msg) = if pop {
                    inbox.complete_msges.pop_front()
                } else {
                    inbox.complete_msges.front().cloned()
                } {
                    self.notify_read(&msg);
                    return msg;
                };
            }

            // Sender can never be dropped since we hold a reference to self which contains the
            // inbox.
            let _ = subscriber.changed().await;
        }
    }

    /// Notify the sender of a message that it has been read.
    fn notify_read(&self, msg: &ReceivedMessage) {
        let mut mp = MessagePacket::new(PacketBuffer::new());
        let mut header = mp.header_mut();
        header.set_message_id(msg.id);
        header.flags_mut().set_read();

        debug!("Notify sender we read message {}", msg.id.as_hex());

        match (msg.src_ip, msg.dst_ip) {
            (IpAddr::V6(src), IpAddr::V6(dst)) => {
                self.data_plane
                    .lock()
                    .unwrap()
                    // IMPORTANT: dst and src are reversed here. src is the sender of the message,
                    // which is the destination of this READ notification.
                    .inject_message_packet(dst, src, mp.into_inner());
            }
            _ => {
                debug!("Can only send messages between two IPv6 addresses")
            }
        };
    }
}

impl<M> Clone for MessageStack<M> {
    fn clone(&self) -> Self {
        Self {
            data_plane: self.data_plane.clone(),
            inbox: self.inbox.clone(),
            outbox: self.outbox.clone(),
            subscriber: self.subscriber.clone(),
            reply_subscribers: self.reply_subscribers.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageInfo {
    /// The receiver of this message.
    pub dst: IpAddr,
    /// Transmission state of the message.
    pub state: TransmissionProgress,
    /// Time the message was created (received) by the system.
    pub created: i64,
    /// Time at which point we will give up sending the message.
    pub deadline: i64,
    /// Size of the message in bytes.
    pub msg_len: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransmissionProgress {
    /// Pending transmission, the remote has not yet acknowledged our init message.
    Pending,
    /// In transit, the remote acknowledged our init message and we are sending chunks.
    Sending {
        /// Chunks which have never been sent.
        pending: usize,
        /// Chunks which have been sent at least once, but haven't been acknowledged.
        sent: usize,
        /// Chunks which have been acknowledged and won't be sent again.
        acked: usize,
    },
    /// The remote acknowledged full reception, including checksum verification.
    Received,
    /// The remote notified us that the message has been read at least once.
    Read,
    /// We aborted sending this message, the remote __might__ have a full message and process it,
    /// but that generally won't be the case.
    Aborted,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId([u8; MESSAGE_ID_SIZE]);

impl MessageId {
    /// Generate a new random `MessageId`.
    fn new() -> Self {
        let mut id = Self([0u8; 8]);

        id.0.fill(&mut rand::rng());

        id
    }

    /// Get a hex representation of the `MessageId`.
    pub fn as_hex(&self) -> String {
        faster_hex::hex_string(&self.0)
    }
}

impl Serialize for MessageId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.as_hex())
    }
}

struct MessageIdVisitor;

impl Visitor<'_> for MessageIdVisitor {
    type Value = MessageId;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("A hex encoded message id (16 characters)")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if v.len() != 16 {
            Err(E::custom("Message ID is 16 characters long"))
        } else {
            let mut backing = [0; 8];
            faster_hex::hex_decode(v.as_bytes(), &mut backing)
                .map_err(|_| E::custom("MessageID is not valid hex"))?;
            Ok(MessageId(backing))
        }
    }
}

impl<'de> Deserialize<'de> for MessageId {
    fn deserialize<D>(deserializer: D) -> Result<MessageId, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(MessageIdVisitor)
    }
}

/// An owned [`PacketBuffer`] for working with messages.
pub struct MessagePacket {
    packet: PacketBuffer,
}

impl MessagePacket {
    /// Create a new `MessagePacket` in the given [`PacketBuffer`].
    pub fn new(mut packet: PacketBuffer) -> Self {
        // Set the size used by the buffer to already be sufficient for the header.
        packet.set_size(MESSAGE_HEADER_SIZE);
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

    /// Sets the size used by the buffer of the `MessagePacket`.
    pub fn set_used_buffer_size(&mut self, size: usize) {
        self.packet.set_size(size + MESSAGE_HEADER_SIZE)
    }

    /// Consumes this `MessagePacket`, returning the underlying [`PacketBuffer`].
    pub fn into_inner(self) -> PacketBuffer {
        self.packet
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

impl Flags<'_> {
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

    /// Check if the MESSAGE_REPLY flag is set on the header.
    fn reply(&self) -> bool {
        self.flags & FLAG_MESSAGE_REPLY != 0
    }

    /// Check if the MESSAGE_ACK flag is set on the header.
    fn ack(&self) -> bool {
        self.flags & FLAG_MESSAGE_ACK != 0
    }
}

impl fmt::Binary for Flags<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{:016b}", self.flags))
    }
}

/// A mutable reference to the flags in a message header.
// We keep a separate struct because creating a u16 from a byte buffer will write the data in
// native endianness, but we need big endian. So we add a drop implementation which forces a big
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

    /// Sets the MESSAGE_REPLY flag on the header.
    fn set_reply(&mut self) {
        self.flags |= FLAG_MESSAGE_REPLY;
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
impl MessagePacketHeader<'_> {
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
    /// Set the [`MessageId`] in the buffer to the provided value.
    fn set_message_id(&mut self, mid: MessageId) {
        self.header[..MESSAGE_ID_SIZE].copy_from_slice(&mid.0[..]);
    }

    /// Get a reference to the [`Flags`] in this header.
    // TODO: fix tests to not use this method
    #[allow(dead_code)]
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

impl Deref for MessagePacketHeader<'_> {
    type Target = [u8; MESSAGE_HEADER_SIZE];

    fn deref(&self) -> &Self::Target {
        self.header
    }
}

impl Deref for MessagePacketHeaderMut<'_> {
    type Target = [u8; MESSAGE_HEADER_SIZE];

    fn deref(&self) -> &Self::Target {
        self.header
    }
}

impl DerefMut for MessagePacketHeaderMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.header
    }
}

#[derive(Clone)]
pub struct Message {
    /// Generated ID, used to identify the message on the wire
    id: MessageId,
    /// Source IP (ours)
    src: IpAddr,
    /// Destination IP
    dst: IpAddr,
    /// An optional topic of the message, useful to differentiate messages before reading.
    topic: Vec<u8>,
    /// Data of the message
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
    /// Calculates the [`MessageChecksum`] of the message.
    ///
    /// Currently this is a 32 byte blake3 hash.
    pub fn checksum(&self) -> MessageChecksum {
        blake3::hash(&self.data)
    }
}

impl fmt::Display for PushMessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TopicTooLarge => f.write_str("topic too large, topic is limited to 255 bytes"),
        }
    }
}

impl std::error::Error for PushMessageError {}

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
    fn set_reply_flag() {
        let mut buf = [0; MESSAGE_HEADER_SIZE];
        let mut buf_mut = MessagePacketHeaderMut { header: &mut buf };
        buf_mut.flags_mut().set_reply();

        assert!(buf_mut.flags().reply());
        assert_eq!(buf_mut.header[8], 0b0000_0100);
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
