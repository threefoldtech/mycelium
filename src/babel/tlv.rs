use super::{hello::Hello, ihu::Ihu, update::Update};

#[derive(Debug, PartialEq, Clone)]
pub enum TlvType {
    /// Hello TLV type as defind in https://datatracker.ietf.org/doc/html/rfc8966#name-hello.
    Hello = 4,
    /// IHU Tlv as defined in https://datatracker.ietf.org/doc/html/rfc8966#section-4.6.6.
    IHU = 5,
    /// An update TLV as defined https://datatracker.ietf.org/doc/html/rfc8966#section-4.6.9.
    Update = 8,
}

/// A single `Tlv` in a babel packet body.
pub enum Tlv {
    /// Hello Tlv type.
    Hello(Hello),
    /// Hello Tlv type.
    Ihu(Ihu),
    /// Hello Tlv type.
    Update(Update),
}
