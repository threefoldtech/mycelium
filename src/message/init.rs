use super::MessagePacket;

/// A message representing an init message.
///
/// The body of an init message has the following structure:
///   - 8 bytes size
pub struct MessageInit {
    buffer: MessagePacket,
}

impl MessageInit {
    /// Create a new `MessageInit` in the provided [`MessagePacket`].
    pub fn new(mut buffer: MessagePacket) -> Self {
        buffer.set_used_buffer_size(9);
        buffer.header_mut().flags_mut().set_init();
        Self { buffer }
    }

    /// Return the length of the message, as written in the body.
    pub fn length(&self) -> u64 {
        u64::from_be_bytes(
            self.buffer.buffer()[..8]
                .try_into()
                .expect("Buffer contains a size field of valid length; qed"),
        )
    }

    /// Return the topic of the message, as written in the body.
    pub fn topic(&self) -> &[u8] {
        let topic_len = self.buffer.buffer()[8] as usize;
        &self.buffer.buffer()[9..9 + topic_len]
    }

    /// Set the length field of the message body.
    pub fn set_length(&mut self, length: u64) {
        self.buffer.buffer_mut()[..8].copy_from_slice(&length.to_be_bytes())
    }

    /// Set the topic in the message body.
    ///
    /// # Panics
    ///
    /// This function panics if the topic is longer than 255 bytes.
    pub fn set_topic(&mut self, topic: &[u8]) {
        assert!(
            topic.len() <= u8::MAX as usize,
            "Topic can be 255 bytes long at most"
        );
        self.buffer.set_used_buffer_size(9 + topic.len());
        self.buffer.buffer_mut()[8] = topic.len() as u8;
        self.buffer.buffer_mut()[9..9 + topic.len()].copy_from_slice(topic);
    }

    /// Convert the `MessageInit` into a reply. This does nothing if it is already a reply.
    pub fn into_reply(mut self) -> Self {
        self.buffer.header_mut().flags_mut().set_ack();
        self
    }

    /// Consumes this `MessageInit`, returning the underlying [`MessagePacket`].
    pub fn into_inner(self) -> MessagePacket {
        self.buffer
    }
}

#[cfg(test)]
mod tests {
    use crate::{crypto::PacketBuffer, message::MessagePacket};

    use super::MessageInit;

    #[test]
    fn init_flag_set() {
        let mi = MessageInit::new(MessagePacket::new(PacketBuffer::new()));

        let mp = mi.into_inner();
        assert!(mp.header().flags().init());
    }

    #[test]
    fn read_length() {
        let mut pb = PacketBuffer::new();
        pb.buffer_mut()[12..20].copy_from_slice(&[0, 0, 0, 0, 2, 3, 4, 5]);

        let ms = MessageInit::new(MessagePacket::new(pb));

        assert_eq!(ms.length(), 33_752_069);
    }

    #[test]
    fn write_length() {
        let mut ms = MessageInit::new(MessagePacket::new(PacketBuffer::new()));

        ms.set_length(3_432_634_632);

        // Since we don't work with packet buffer we don't have to account for the message packet
        // header.
        assert_eq!(&ms.buffer.buffer()[..8], &[0, 0, 0, 0, 204, 153, 217, 8]);
        assert_eq!(ms.length(), 3_432_634_632);
    }
}
