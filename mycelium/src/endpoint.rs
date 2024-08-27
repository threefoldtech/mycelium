use std::{
    fmt,
    net::{AddrParseError, SocketAddr},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Error generated while processing improperly formatted endpoints.
pub enum EndpointParseError {
    /// An address was specified without leading protocol information.
    MissingProtocol,
    /// An endpoint was specified using a protocol we (currently) do not understand.
    UnknownProtocol,
    /// Error while parsing the specific address.
    Address(AddrParseError),
}

/// Protocol used by an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    /// Standard plain text Tcp.
    Tcp,
    /// Tls 1.3 with PSK over Tcp.
    Tls,
    /// Quic protocol (over UDP).
    Quic,
}

/// An endpoint defines a address and a protocol to use when communicating with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    proto: Protocol,
    socket_addr: SocketAddr,
}

impl Endpoint {
    /// Create a new `Endpoint` with given [`Protocol`] and address.
    pub fn new(proto: Protocol, socket_addr: SocketAddr) -> Self {
        Self { proto, socket_addr }
    }

    /// Get the [`Protocol`] used by this `Endpoint`.
    pub fn proto(&self) -> Protocol {
        self.proto
    }

    /// Get the [`SocketAddr`] used by this `Endpoint`.
    pub fn address(&self) -> SocketAddr {
        self.socket_addr
    }
}

impl FromStr for Endpoint {
    type Err = EndpointParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once("://") {
            None => Err(EndpointParseError::MissingProtocol),
            Some((proto, socket)) => {
                let proto = match proto.to_lowercase().as_str() {
                    "tcp" => Protocol::Tcp,
                    "quic" => Protocol::Quic,
                    "tls" => Protocol::Tls,
                    _ => return Err(EndpointParseError::UnknownProtocol),
                };
                let socket_addr = SocketAddr::from_str(socket)?;
                Ok(Endpoint { proto, socket_addr })
            }
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{} {}", self.proto, self.socket_addr))
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tcp => "Tcp",
            Self::Tls => "Tls",
            Self::Quic => "Quic",
        })
    }
}

impl fmt::Display for EndpointParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProtocol => f.write_str("missing leading protocol identifier"),
            Self::UnknownProtocol => f.write_str("protocol for endpoint is not supported"),
            Self::Address(e) => f.write_fmt(format_args!("failed to parse address: {}", e)),
        }
    }
}

impl std::error::Error for EndpointParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Address(e) => Some(e),
            _ => None,
        }
    }
}

impl From<AddrParseError> for EndpointParseError {
    fn from(value: AddrParseError) -> Self {
        Self::Address(value)
    }
}
