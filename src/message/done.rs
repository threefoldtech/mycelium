use super::{MessageChecksum, MessagePacket, MESSAGE_CHECKSUM_LENGTH};

/// A message representing a "done" message.
///
/// The body of a done message has the following structure:
///   - 8 bytes: chunks transmitted
///   - 32 bytes: checksum of the transmitted data
pub struct MessageDone {
    buffer: MessagePacket,
}

impl MessageDone {
    /// Create a new `MessageDone` in the provided [`MessagePacket`].
    pub fn new(mut buffer: MessagePacket) -> Self {
        buffer.set_used_buffer_size(40);
        buffer.header_mut().flags_mut().set_done();
        Self { buffer }
    }

    /// Return the amount of chunks in the message, as written in the body.
    pub fn chunk_count(&self) -> u64 {
        u64::from_be_bytes(
            self.buffer.buffer()[..8]
                .try_into()
                .expect("Buffer contains a size field of valid length; qed"),
        )
    }

    /// Set the amount of chunks field of the message body.
    pub fn set_chunk_count(&mut self, chunk_count: u64) {
        self.buffer.buffer_mut()[..8].copy_from_slice(&chunk_count.to_be_bytes())
    }

    /// Get the checksum of the message from the body.
    pub fn checksum(&self) -> MessageChecksum {
        MessageChecksum::from_bytes(
            self.buffer.buffer()[8..8 + MESSAGE_CHECKSUM_LENGTH]
                .try_into()
                .expect("Buffer contains enough data for a checksum; qed"),
        )
    }

    /// Set the checksum of the message in the body.
    pub fn set_checksum(&mut self, checksum: MessageChecksum) {
        self.buffer.buffer_mut()[8..8 + MESSAGE_CHECKSUM_LENGTH]
            .copy_from_slice(checksum.as_bytes())
    }

    /// Convert the `MessageDone` into a reply. This does nothing if it is already a reply.
    pub fn into_reply(mut self) -> Self {
        self.buffer.header_mut().flags_mut().set_ack();
        self
    }

    /// Consumes this `MessageDone`, returning the underlying [`MessagePacket`].
    pub fn into_inner(self) -> MessagePacket {
        self.buffer
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        crypto::PacketBuffer,
        message::{MessageChecksum, MessagePacket},
    };

    use super::MessageDone;

    #[test]
    fn done_flag_set() {
        let md = MessageDone::new(MessagePacket::new(PacketBuffer::new()));

        let mp = md.into_inner();
        assert!(mp.header().flags().done());
    }

    #[test]
    fn read_chunk_count() {
        let mut pb = PacketBuffer::new();
        pb.buffer_mut()[12..20].copy_from_slice(&[0, 0, 0, 0, 0, 0, 73, 55]);

        let ms = MessageDone::new(MessagePacket::new(pb));

        assert_eq!(ms.chunk_count(), 18_743);
    }

    #[test]
    fn write_chunk_count() {
        let mut ms = MessageDone::new(MessagePacket::new(PacketBuffer::new()));

        ms.set_chunk_count(10_000);

        // Since we don't work with packet buffer we don't have to account for the message packet
        // header.
        assert_eq!(&ms.buffer.buffer()[..8], &[0, 0, 0, 0, 0, 0, 39, 16]);
        assert_eq!(ms.chunk_count(), 10_000);
    }

    #[test]
    fn read_checksum() {
        const CHECKSUM: MessageChecksum = MessageChecksum::from_bytes([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B,
            0x1C, 0x1D, 0x1E, 0x1F,
        ]);
        let mut pb = PacketBuffer::new();
        pb.buffer_mut()[20..52].copy_from_slice(CHECKSUM.as_bytes());

        let ms = MessageDone::new(MessagePacket::new(pb));

        assert_eq!(ms.checksum(), CHECKSUM);
    }

    #[test]
    fn write_checksum() {
        const CHECKSUM: MessageChecksum = MessageChecksum::from_bytes([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B,
            0x1C, 0x1D, 0x1E, 0x1F,
        ]);
        let mut ms = MessageDone::new(MessagePacket::new(PacketBuffer::new()));

        ms.set_checksum(CHECKSUM);

        // Since we don't work with packet buffer we don't have to account for the message packet
        // header.
        assert_eq!(&ms.buffer.buffer()[8..40], CHECKSUM.as_bytes());
        assert_eq!(ms.checksum(), CHECKSUM);
    }
}
