//! This module contains babel related structs.
//!
//! We don't fully implement the babel spec, and items which are implemented might deviate to fit
//! our specific use case. For reference, the implementation is based on [this
//! RFC](https://datatracker.ietf.org/doc/html/rfc8966).

use std::io;

use bytes::{Buf, BufMut};
use tokio_util::codec::{Decoder, Encoder};
use tracing::trace;

pub use self::{
    hello::Hello, ihu::Ihu, route_request::RouteRequest, seqno_request::SeqNoRequest,
    update::Update,
};

pub use self::tlv::Tlv;

mod hello;
mod ihu;
mod route_request;
mod seqno_request;
mod tlv;
mod update;

/// Magic byte to identify babel protocol packet.
const BABEL_MAGIC: u8 = 42;
/// The version of the protocol we are currently using.
const BABEL_VERSION: u8 = 2;

/// Size of a babel header on the wire.
const HEADER_WIRE_SIZE: usize = 4;

/// TLV type for the [`Hello`] tlv
const TLV_TYPE_HELLO: u8 = 4;
/// TLV type for the [`Ihu`] tlv
const TLV_TYPE_IHU: u8 = 5;
/// TLV type for the [`Update`] tlv
const TLV_TYPE_UPDATE: u8 = 8;
/// TLV type for the [`RouteRequest`] tlv
const TLV_TYPE_ROUTE_REQUEST: u8 = 9;
/// TLV type for the [`SeqNoRequest`] tlv
const TLV_TYPE_SEQNO_REQUEST: u8 = 10;

/// Wildcard address, the value is empty (0 bytes length).
const AE_WILDCARD: u8 = 0;
/// IPv4 address, the value is _at most_ 4 bytes long.
const AE_IPV4: u8 = 1;
/// IPv6 address, the value is _at most_ 16 bytes long.
const AE_IPV6: u8 = 2;
/// Link-local IPv6 address, the value is 8 bytes long. This implies a `fe80::/64` prefix.
const AE_IPV6_LL: u8 = 3;

/// A codec which can send and receive whole babel packets on the wire.
#[derive(Debug, Clone)]
pub struct Codec {
    header: Option<Header>,
}

impl Codec {
    /// Create a new `BabelCodec`.
    pub fn new() -> Self {
        Self { header: None }
    }

    /// Resets the `BabelCodec` to its default state.
    pub fn reset(&mut self) {
        self.header = None;
    }
}

/// The header for a babel packet. This follows the definition of the header [in the
/// RFC](https://datatracker.ietf.org/doc/html/rfc8966#name-packet-format). Since the header
/// contains only hard-coded fields and the length of an encoded body, there is no need for users
/// to manually construct this. In fact, it exists only to make our lives slightly easier in
/// reading/writing the header on the wire.
#[derive(Debug, Clone)]
struct Header {
    magic: u8,
    version: u8,
    /// This is the length of the whole body following this header. Also excludes any possible
    /// trailers.
    body_length: u16,
}

impl Decoder for Codec {
    type Item = Tlv;

    type Error = io::Error;

    fn decode(&mut self, src: &mut bytes::BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Read a header if we don't have one yet.
        let header = if let Some(header) = self.header.take() {
            trace!("Continue from stored header");
            header
        } else {
            if src.remaining() < HEADER_WIRE_SIZE {
                trace!("Insufficient bytes to read a babel header");
                return Ok(None);
            }

            trace!("Read babel header");

            Header {
                magic: src.get_u8(),
                version: src.get_u8(),
                body_length: src.get_u16(),
            }
        };

        if src.remaining() < header.body_length as usize {
            trace!("Insufficient bytes to read babel body");
            self.header = Some(header);
            return Ok(None);
        }

        // Siltently ignore packets which don't have the correct values set, as defined in the
        // spec. Note that we consume the amount of bytes indentified so we leave the parser in the
        // correct state for the next packet.
        if header.magic != BABEL_MAGIC || header.version != BABEL_VERSION {
            trace!("Dropping babel packet with wrong magic or version");
            src.advance(header.body_length as usize);
            self.reset();
            return Ok(None);
        }

        // at this point we have a whole body loaded in the buffer. We currently don't support sub
        // TLV's

        trace!("Read babel TLV body");

        // TODO: Technically we need to loop here as we can have multiple TLVs.

        // TLV header
        let tlv_type = src.get_u8();
        let body_len = src.get_u8();
        // TLV payload
        let tlv = match tlv_type {
            TLV_TYPE_HELLO => Some(Hello::from_bytes(src).into()),
            TLV_TYPE_IHU => Ihu::from_bytes(src, body_len).map(From::from),
            TLV_TYPE_UPDATE => Update::from_bytes(src, body_len).map(From::from),
            TLV_TYPE_ROUTE_REQUEST => RouteRequest::from_bytes(src, body_len).map(From::from),
            TLV_TYPE_SEQNO_REQUEST => SeqNoRequest::from_bytes(src, body_len).map(From::from),
            _ => {
                // unrecoginized body type, silently drop
                trace!("Dropping unrecognized tlv");
                // We already read 2 bytes
                src.advance(header.body_length as usize - 2);
                self.reset();
                return Ok(None);
            }
        };

        Ok(tlv)
    }
}

impl Encoder<Tlv> for Codec {
    type Error = io::Error;

    fn encode(&mut self, item: Tlv, dst: &mut bytes::BytesMut) -> Result<(), Self::Error> {
        // Write header
        dst.put_u8(BABEL_MAGIC);
        dst.put_u8(BABEL_VERSION);
        dst.put_u16(item.wire_size() as u16 + 2); // tlv payload + tlv header

        // Write TLV's, TODO: currently only 1 TLV/body

        // TLV header
        match item {
            Tlv::Hello(_) => dst.put_u8(TLV_TYPE_HELLO),
            Tlv::Ihu(_) => dst.put_u8(TLV_TYPE_IHU),
            Tlv::Update(_) => dst.put_u8(TLV_TYPE_UPDATE),
            Tlv::RouteRequest(_) => dst.put_u8(TLV_TYPE_ROUTE_REQUEST),
            Tlv::SeqNoRequest(_) => dst.put_u8(TLV_TYPE_SEQNO_REQUEST),
        }
        dst.put_u8(item.wire_size());
        item.write_bytes(dst);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv6Addr, time::Duration};

    use futures::{SinkExt, StreamExt};
    use tokio_util::codec::Framed;

    use crate::subnet::Subnet;

    #[tokio::test]
    async fn codec_hello() {
        let (tx, rx) = tokio::io::duplex(1024);
        let mut sender = Framed::new(tx, super::Codec::new());
        let mut receiver = Framed::new(rx, super::Codec::new());

        let hello = super::Hello::new_unicast(15.into(), 400);

        sender
            .send(hello.clone().into())
            .await
            .expect("Send on a non-networked buffer can never fail; qed");
        let recv_hello = receiver
            .next()
            .await
            .expect("Buffer isn't closed so this is always `Some`; qed")
            .expect("Can decode the previously encoded value");
        assert_eq!(super::Tlv::from(hello), recv_hello);
    }

    #[tokio::test]
    async fn codec_ihu() {
        let (tx, rx) = tokio::io::duplex(1024);
        let mut sender = Framed::new(tx, super::Codec::new());
        let mut receiver = Framed::new(rx, super::Codec::new());

        let ihu = super::Ihu::new(27.into(), 400, None);

        sender
            .send(ihu.clone().into())
            .await
            .expect("Send on a non-networked buffer can never fail; qed");
        let recv_ihu = receiver
            .next()
            .await
            .expect("Buffer isn't closed so this is always `Some`; qed")
            .expect("Can decode the previously encoded value");
        assert_eq!(super::Tlv::from(ihu), recv_ihu);
    }

    #[tokio::test]
    async fn codec_update() {
        let (tx, rx) = tokio::io::duplex(1024);
        let mut sender = Framed::new(tx, super::Codec::new());
        let mut receiver = Framed::new(rx, super::Codec::new());

        let update = super::Update::new(
            Duration::from_secs(400),
            16.into(),
            25.into(),
            Subnet::new(Ipv6Addr::new(0x400, 1, 2, 3, 0, 0, 0, 0).into(), 64)
                .expect("64 is a valid IPv6 prefix size; qed"),
            [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
            ]
            .into(),
        );

        sender
            .send(update.clone().into())
            .await
            .expect("Send on a non-networked buffer can never fail; qed");
        println!("Sent update packet");
        let recv_update = receiver
            .next()
            .await
            .expect("Buffer isn't closed so this is always `Some`; qed")
            .expect("Can decode the previously encoded value");
        println!("Received update packet");
        assert_eq!(super::Tlv::from(update), recv_update);
    }

    #[tokio::test]
    async fn codec_seqno_request() {
        let (tx, rx) = tokio::io::duplex(1024);
        let mut sender = Framed::new(tx, super::Codec::new());
        let mut receiver = Framed::new(rx, super::Codec::new());

        let snr = super::SeqNoRequest::new(
            16.into(),
            [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
                24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
            ]
            .into(),
            Subnet::new(Ipv6Addr::new(0x400, 1, 2, 3, 0, 0, 0, 0).into(), 64)
                .expect("64 is a valid IPv6 prefix size; qed"),
        );

        sender
            .send(snr.clone().into())
            .await
            .expect("Send on a non-networked buffer can never fail; qed");
        let recv_update = receiver
            .next()
            .await
            .expect("Buffer isn't closed so this is always `Some`; qed")
            .expect("Can decode the previously encoded value");
        assert_eq!(super::Tlv::from(snr), recv_update);
    }

    #[tokio::test]
    async fn codec_route_request() {
        let (tx, rx) = tokio::io::duplex(1024);
        let mut sender = Framed::new(tx, super::Codec::new());
        let mut receiver = Framed::new(rx, super::Codec::new());

        let rr = super::RouteRequest::new(Some(
            Subnet::new(Ipv6Addr::new(0x400, 1, 2, 3, 0, 0, 0, 0).into(), 64)
                .expect("64 is a valid IPv6 prefix size; qed"),
        ));

        sender
            .send(rr.clone().into())
            .await
            .expect("Send on a non-networked buffer can never fail; qed");
        let recv_update = receiver
            .next()
            .await
            .expect("Buffer isn't closed so this is always `Some`; qed")
            .expect("Can decode the previously encoded value");
        assert_eq!(super::Tlv::from(rr), recv_update);
    }
}
