//! This module contains babel related structs.
//!
//! We don't fully implement the babel spec, and items which are implemented might deviate to fit
//! our specific use case. For reference, the implementation is based on [this
//! RFC](https://datatracker.ietf.org/doc/html/rfc8966).

use self::tlv::TlvType;

mod hello;
mod ihu;
mod tlv;
mod update;

/// Magic byte to identify babel protocol packet.
const BABEL_MAGIC: u8 = 42;
/// The version of the protocol we are currently using.
const BABEL_VERSION: u8 = 2;

/// Address encoding in the babel protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressEncoding {
    /// Wildcard address, the value is empty (0 bytes length).
    Wildcard = 0,
    /// IPv4 address, the value is _at most_ 4 bytes long.
    IPv4 = 1,
    /// IPv6 address, the value is _at most_ 16 bytes long.
    IPv6 = 2,
    /// Link-local IPv6 address, the value is 8 bytes long. This implies a `fe80::/64` prefix.
    IPv6LL = 3,
}

/// The header for a babel packet. This follows the definition of the header [in the
/// RFC](https://datatracker.ietf.org/doc/html/rfc8966#name-packet-format). Since the header
/// contains only hard-coded fields and the length of an encoded body, there is no need for users
/// to manually construct this. In fact, it exists only to make our lives slightly easier in
/// reading/writing the header on the wire.
struct Header {
    magic: u8,
    version: u8,
    /// This is the length of the whole body following this header. Also excludes any possible
    /// trailers.
    body_length: u16,
}

/// The header for a babel packet. This follows the definition of the body [in the
/// RFC](https://datatracker.ietf.org/doc/html/rfc8966#name-packet-format). Since the body fields
/// are derived from the actual TLV('s), there is no need for users to manually construct this. In
/// fact, it exists only to make our lives slightly easier in reading/writing the body on the wire.
struct Body {
    tlv_type: TlvType,
    length: u8, // length of the tlv (only the tlv, not tlv_type and length itself)
    tlv: crate::packet::BabelTLV,
}
