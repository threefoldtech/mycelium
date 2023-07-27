//! The babel [IHU TLV](https://datatracker.ietf.org/doc/html/rfc8966#name-ihu).

use std::net::IpAddr;

use crate::metric::Metric;

/// IHU TLV body as defined in https://datatracker.ietf.org/doc/html/rfc8966#name-ihu.
#[derive(Debug, Clone)]
pub struct Ihu {
    rx_cost: Metric,
    interval: u16,
    address: IpAddr,
}

impl Ihu {
    /// Create a new `Ihu` to be transmitted.
    pub fn new(rx_cost: Metric, interval: u16, address: IpAddr) -> Self {
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
    pub fn address(&self) -> IpAddr {
        self.address
    }
}
