// As an example, consider a protocol that can be used to send strings where each frame is 
// a four byte integer that contains the length of the frame, followed by that many bytes of string data. 
// The decoder fails with an error if the string data is not valid utf-8 or too long.

use tokio_util::codec::Decoder;
use bytes::{BytesMut, Buf};

pub struct MyStringDecoder{}

const MAX: usize = 8 * 1024 * 1024;

impl Decoder for MyStringDecoder {
    type Item = String;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            // Not enough data to read length marker.
            return Ok(None);
        }

        // Read length marker
        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let length = u32::from_le_bytes(length_bytes) as usize;

        // Check that the length is not too large to avoid a denial of
        // service attack where the server runs out of memory.
        if length > MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", length)
            ));
        }

        if src.len() < 4 + length {
            // The full string has not yet arrived.
            //
            // We reserve more place in the buffer. This is not strictly
            // necessary, but is a good idea performance-wise.
            src.reserve(4 + length - src.len());

            // We inform the Framed that we need more bytes to form the next
            // frame
            return Ok(None);
        }

        // Use advance to modify src such that it no longer contains
        // this frame.
        let data = src[4..4 + length].to_vec();
        src.advance(4 + length);

        // Convert the data to a string, or fail if it is not valid utf-8.
        match String::from_utf8(data) {
            Ok(string) => Ok(Some(string)),
            Err(utf8_error) => {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    utf8_error.utf8_error(),
                ))
            },
        }
    }
}

// As an example, consider a protocol that can be used to send strings where each frame is 
// a four byte integer that contains the length of the frame, followed by that many bytes of string data. 
// The encoder will fail if the string is too long.

use tokio_util::codec::Encoder;

pub struct MyStringEncoder {}

impl Encoder<String> for MyStringEncoder {
    type Error = std::io::Error;

    fn encode(&mut self, item: String, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // Don't send a string if it is longer than the other end will
        // accept.
        if item.len() > MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Frame of length {} is too large.", item.len())
            ));
        }

        // Convert the length into a byte array.
        // The cast to u32 cannot overflow due to the length check above.
        let len_slice = u32::to_le_bytes(item.len() as u32);

        // Reserve space in the buffer
        dst.reserve(4 + item.len());

        // Write the length and the string to the buffer
        dst.extend_from_slice(&len_slice);
        dst.extend_from_slice(&item.as_bytes());
        Ok(())
    }
}