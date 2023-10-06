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
    pub fn new(buffer: MessagePacket) -> Self {
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

    /// Set the length field of the message body.
    pub fn set_length(&mut self, length: u64) {
        self.buffer.buffer_mut()[..8].copy_from_slice(&length.to_be_bytes())
    }
}

#[cfg(test)]
mod tests {
    use crate::{crypto::PacketBuffer, message::MessagePacket};

    use super::MessageInit;

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
