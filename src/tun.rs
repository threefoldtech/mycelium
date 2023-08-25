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

impl Deref for IpPacket {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
