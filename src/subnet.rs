//! A dedicated subnet module.
//!
//! The standard library only exposes [`IpAddress`](std::net::IpAddress), and types related to
//! specific IPv4 and IPv6 addresses. It does not however, expose dedicated types to represents
//! appropriate subnets.
//!
//! This code is not meant to fully support subnets, but rather only the subset as needed by the
//! main application code. As such, this implementation is optimized for the specific use case, and
//! might not be optimal for other uses.

use core::fmt;
use std::net::IpAddr;

use ipnet::IpNet;

/// Representation of a subnet. A subnet can be either IPv4 or IPv6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Subnet {
    inner: IpNet,
}

/// An error returned when creating a new [`Subnet`] with an invalid prefix length.
///
/// For IPv4, the max prefix lenght is 32, and for IPv6 it is 128;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixLenError;

impl Subnet {
    /// Create a new `Subnet` from the given [`IpAddr`] and prefix length.
    pub fn new(addr: IpAddr, prefix_len: u8) -> Result<Subnet, PrefixLenError> {
        Ok(Self {
            inner: IpNet::new(addr, prefix_len).map_err(|_| PrefixLenError)?,
        })
    }

    /// Returns the size of the prefix in bits.
    pub fn prefix_len(&self) -> u8 {
        self.inner.prefix_len()
    }

    /// Retuns the address in this subnet.
    pub fn address(&self) -> IpAddr {
        self.inner.addr()
    }
}

impl fmt::Display for Subnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inner)
    }
}

impl fmt::Display for PrefixLenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Invalid prefix length for this address")
    }
}

impl std::error::Error for PrefixLenError {}
