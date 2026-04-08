use std::{
    fmt,
    net::{AddrParseError, SocketAddr},
    num::ParseIntError,
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
    /// Error while parsing a vsock address component.
    VsockAddress(ParseIntError),
    /// A vsock address is missing the required CID:PORT format.
    MissingVsockPort,
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
    /// VM socket (vsock) for hypervisor-VM communication.
    Vsock,
}

/// Address of a peer — either a network socket address or a vsock (CID, port) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PeerAddr {
    /// Standard IPv4 or IPv6 socket address.
    Socket(SocketAddr),
    /// Vsock address identified by a context ID and port.
    Vsock { cid: u32, port: u32 },
}

impl PeerAddr {
    /// Return the inner [`SocketAddr`] if this is a socket address, otherwise `None`.
    pub fn socket_addr(self) -> Option<SocketAddr> {
        match self {
            PeerAddr::Socket(sa) => Some(sa),
            PeerAddr::Vsock { .. } => None,
        }
    }
}

impl fmt::Display for PeerAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerAddr::Socket(sa) => sa.fmt(f),
            PeerAddr::Vsock { cid, port } => write!(f, "{cid}:{port}"),
        }
    }
}

/// An endpoint defines a address and a protocol to use when communicating with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    proto: Protocol,
    addr: PeerAddr,
}

impl Endpoint {
    /// Create a new `Endpoint` with given [`Protocol`] and socket address.
    pub fn new(proto: Protocol, socket_addr: SocketAddr) -> Self {
        Self {
            proto,
            addr: PeerAddr::Socket(socket_addr),
        }
    }

    /// Create a new vsock `Endpoint` with the given CID and port.
    pub fn new_vsock(cid: u32, port: u32) -> Self {
        Self {
            proto: Protocol::Vsock,
            addr: PeerAddr::Vsock { cid, port },
        }
    }

    /// Get the [`Protocol`] used by this `Endpoint`.
    pub fn proto(&self) -> Protocol {
        self.proto
    }

    /// Get the [`PeerAddr`] used by this `Endpoint`.
    pub fn address(&self) -> PeerAddr {
        self.addr
    }

    /// Get the inner [`SocketAddr`] if this endpoint uses a socket address, otherwise `None`.
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.addr.socket_addr()
    }
}

impl FromStr for Endpoint {
    type Err = EndpointParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once("://") {
            None => Err(EndpointParseError::MissingProtocol),
            Some((proto, addr)) => {
                let proto = match proto.to_lowercase().as_str() {
                    "tcp" => Protocol::Tcp,
                    "quic" => Protocol::Quic,
                    "tls" => Protocol::Tls,
                    "vsock" => Protocol::Vsock,
                    _ => return Err(EndpointParseError::UnknownProtocol),
                };
                if proto == Protocol::Vsock {
                    let (cid_str, port_str) = addr
                        .split_once(':')
                        .ok_or(EndpointParseError::MissingVsockPort)?;
                    let cid = cid_str
                        .parse::<u32>()
                        .map_err(EndpointParseError::VsockAddress)?;
                    let port = port_str
                        .parse::<u32>()
                        .map_err(EndpointParseError::VsockAddress)?;
                    Ok(Endpoint {
                        proto,
                        addr: PeerAddr::Vsock { cid, port },
                    })
                } else {
                    let socket_addr = SocketAddr::from_str(addr)?;
                    Ok(Endpoint {
                        proto,
                        addr: PeerAddr::Socket(socket_addr),
                    })
                }
            }
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{} {}", self.proto, self.addr))
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tcp => "Tcp",
            Self::Tls => "Tls",
            Self::Quic => "Quic",
            Self::Vsock => "Vsock",
        })
    }
}

impl fmt::Display for EndpointParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProtocol => f.write_str("missing leading protocol identifier"),
            Self::UnknownProtocol => f.write_str("protocol for endpoint is not supported"),
            Self::Address(e) => f.write_fmt(format_args!("failed to parse address: {e}")),
            Self::VsockAddress(e) => {
                f.write_fmt(format_args!("failed to parse vsock address component: {e}"))
            }
            Self::MissingVsockPort => f.write_str("vsock address must be in CID:PORT format"),
        }
    }
}

impl std::error::Error for EndpointParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Address(e) => Some(e),
            Self::VsockAddress(e) => Some(e),
            _ => None,
        }
    }
}

impl From<AddrParseError> for EndpointParseError {
    fn from(value: AddrParseError) -> Self {
        Self::Address(value)
    }
}
