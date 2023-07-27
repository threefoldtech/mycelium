//! The babel [IHU TLV](https://datatracker.ietf.org/doc/html/rfc8966#name-ihu).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use bytes::Buf;
use log::trace;

use crate::metric::Metric;

use super::{AE_IPV4, AE_IPV6, AE_IPV6_LL, AE_WILDCARD};

/// Base wire size of an [`Ihu`] without variable lenght address encoding.
const IHU_BASE_WIRE_SIZE: u16 = 6;

/// IHU TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#name-ihu.
#[derive(Debug, Clone)]
pub struct Ihu {
    rx_cost: Metric,
    interval: u16,
    address: Option<IpAddr>,
}

impl Ihu {
    /// Create a new `Ihu` to be transmitted.
    pub fn new(rx_cost: Metric, interval: u16, address: Option<IpAddr>) -> Self {
        // An interval of 0 is illegal according to the RFC, as this value is used by the receiver
        // to calculate the hold time.
        if interval == 0 {
            panic!("Ihu interval MUST NOT be 0");
        }
        Self {
            rx_cost,
            interval,
            address,
        }
    }

    /// The upper boud in centiseconds after which the sending node will send a new `Ihu`.
    pub fn interval(&self) -> u16 {
        self.interval
    }

    /// The cost of the link according to the sending `Peer`.
    pub fn rx_cost(&self) -> Metric {
        self.rx_cost
    }

    /// The address in this `Ihu`. This is the address of the receiving `Peer`.
    pub fn address(&self) -> Option<IpAddr> {
        self.address
    }

    /// Calculates the size on the wire of this `Ihu`.
    pub fn wire_size(&self) -> u16 {
        IHU_BASE_WIRE_SIZE
            + match self.address {
                None => 0,
                Some(IpAddr::V4(_)) => 4,
                // TODO: link local should be encoded differently
                Some(IpAddr::V6(_)) => 16,
            }
    }

    /// Construct a `Ihu` from wire bytes.
    ///
    /// # Panics
    ///
    /// This function will panic if there are insufficient bytes present in the provided buffer to
    /// decode a complete `Ihu`.
    pub fn from_bytes(src: &mut bytes::BytesMut, len: u8) -> Option<Self> {
        let ae = src.get_u8();
        // read and ignore reserved byte
        let _ = src.get_u8();
        let rx_cost = src.get_u16().into();
        let interval = src.get_u16();
        let address = match ae {
            AE_WILDCARD => None,
            AE_IPV4 => {
                Some(Ipv4Addr::new(src.get_u8(), src.get_u8(), src.get_u8(), src.get_u8()).into())
            }
            AE_IPV6 => Some(
                Ipv6Addr::new(
                    src.get_u16(),
                    src.get_u16(),
                    src.get_u16(),
                    src.get_u16(),
                    src.get_u16(),
                    src.get_u16(),
                    src.get_u16(),
                    src.get_u16(),
                )
                .into(),
            ),
            AE_IPV6_LL => Some(
                Ipv6Addr::new(
                    0xfe80,
                    0,
                    0,
                    0,
                    src.get_u16(),
                    src.get_u16(),
                    src.get_u16(),
                    src.get_u16(),
                )
                .into(),
            ),
            _ => {
                // Invalid AE type, skip reamining data and ignore
                trace!("Invalid AE type in IHU TLV, drop TLV");
                src.advance(len as usize - 6);
                return None;
            }
        };

        Some(Self {
            rx_cost,
            interval,
            address,
        })
    }
}
