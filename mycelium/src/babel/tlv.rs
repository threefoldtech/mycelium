pub use super::{hello::Hello, ihu::Ihu, update::Update};
use super::{route_request::RouteRequest, SeqNoRequest};

/// A single `Tlv` in a babel packet body.
#[derive(Debug, Clone, PartialEq)]
pub enum Tlv {
    /// Hello Tlv type.
    Hello(Hello),
    /// Ihu Tlv type.
    Ihu(Ihu),
    /// Update Tlv type.
    Update(Update),
    /// RouteRequest Tlv type.
    RouteRequest(RouteRequest),
    /// SeqNoRequest Tlv type
    SeqNoRequest(SeqNoRequest),
}

impl Tlv {
    /// Calculate the size on the wire for this `Tlv`. This DOES NOT included the TLV header size
    /// (2 bytes).
    pub fn wire_size(&self) -> u8 {
        match self {
            Self::Hello(hello) => hello.wire_size(),
            Self::Ihu(ihu) => ihu.wire_size(),
            Self::Update(update) => update.wire_size(),
            Self::RouteRequest(route_request) => route_request.wire_size(),
            Self::SeqNoRequest(seqno_request) => seqno_request.wire_size(),
        }
    }

    /// Encode this `Tlv` as part of a packet.
    pub fn write_bytes(&self, dst: &mut bytes::BytesMut) {
        match self {
            Self::Hello(hello) => hello.write_bytes(dst),
            Self::Ihu(ihu) => ihu.write_bytes(dst),
            Self::Update(update) => update.write_bytes(dst),
            Self::RouteRequest(route_request) => route_request.write_bytes(dst),
            Self::SeqNoRequest(seqno_request) => seqno_request.write_bytes(dst),
        }
    }
}

impl From<SeqNoRequest> for Tlv {
    fn from(v: SeqNoRequest) -> Self {
        Self::SeqNoRequest(v)
    }
}

impl From<RouteRequest> for Tlv {
    fn from(v: RouteRequest) -> Self {
        Self::RouteRequest(v)
    }
}

impl From<Update> for Tlv {
    fn from(v: Update) -> Self {
        Self::Update(v)
    }
}

impl From<Ihu> for Tlv {
    fn from(v: Ihu) -> Self {
        Self::Ihu(v)
    }
}

impl From<Hello> for Tlv {
    fn from(v: Hello) -> Self {
        Self::Hello(v)
    }
}
