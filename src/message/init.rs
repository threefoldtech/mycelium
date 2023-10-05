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

    /// Return the length of the ,essage, as written in the body.
    pub fn length(&self) -> u64 {
        u64::from_be_bytes(
            self.buffer.buffer()[..8]
                .try_into()
                .expect("Buffer contains a size field of valid length; qed"),
        )
    }

    /// Set the length field of the message body
    pub fn set_length(&mut self, length: u64) {
        self.buffer.buffer_mut()[..8].copy_from_slice(&length.to_be_bytes())
    }
}
