//! The tun module implements a platform independent Tun interface.

#[cfg(target_os = "linux")]
mod linux;

use std::{io, ops::Deref};

use bytes::{Buf, BufMut};
use etherparse::{IpHeader, ReadError};
use tokio_util::codec::{Decoder, Encoder};

#[cfg(target_os = "linux")]
pub use linux::{new, RxHalf, TxHalf};

/// An IpPacket represents a layer 3 packet.
#[derive(Debug, Clone)]
pub struct IpPacket(Vec<u8>);

impl From<Vec<u8>> for IpPacket {
    fn from(value: Vec<u8>) -> Self {
        IpPacket(value)
    }
}

/// A codec for [`IpPacket`]. This is only a convenience type since we can't implement the
/// [`Decoder`] trait for ().
#[derive(Debug)]
struct IpPacketCodec {
    _p: (),
}

impl IpPacketCodec {
    /// Instantiate a new codec.
    fn new() -> Self {
        Self { _p: () }
    }
}

impl Encoder<IpPacket> for IpPacketCodec {
    // std::convert::Infallible does not implement Into<std::io::Error> so we can't use that here.
    type Error = io::Error;

    fn encode(&mut self, item: IpPacket, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        Ok(dst.put_slice(&item.0))
    }
}

impl Decoder for IpPacketCodec {
    type Item = IpPacket;

    type Error = io::Error;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Only try to decode in case we actually have sufficient data for a header: 20 bytes (in
        // case of IPv4)
        if src.remaining() < 20 {
            return Ok(None);
        }

        // Decode a header, this does not advance the buffer yet.
        let (header, _, _) = match etherparse::IpHeader::from_slice(&src) {
            Ok(h) => h,
            Err(ReadError::UnexpectedEndOfSlice(_)) => return Ok(None),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "can't parse IP header",
                ))
            }
        };

        //   .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "can't parse IP header"))?;
        let size = match header {
            IpHeader::Version4(hdr, _) => hdr.total_len(),
            IpHeader::Version6(hdr, _) => hdr.payload_length + hdr.header_len() as u16,
        } as usize;

        if src.remaining() < size {
            return Ok(None);
        }

        let mut packet = vec![0; size];
        packet.copy_from_slice(&src[..size]);
        src.advance(size);

        Ok(Some(IpPacket(packet)))
    }
}

impl Deref for IpPacket {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
