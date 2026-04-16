//! Default bootstrap peers for the Mycelium public network.
//! Edit this file to update the well-known bootstrap nodes.
//! Both TCP and QUIC on port 9651.

/// Endpoint strings for all default bootstrap peers.
pub const BOOTSTRAP_PEERS: &[&str] = &[
    // DE
    "tcp://188.40.132.242:9651",
    "tcp://136.243.47.186:9651",
    // BE
    "tcp://185.69.166.7:9651",
    "tcp://185.69.166.8:9651",
    // FI
    "tcp://65.21.231.58:9651",
    "tcp://65.109.18.113:9651",
    // US-EAST
    "tcp://209.159.146.190:9651",
    // US-WEST
    "tcp://5.78.122.16:9651",
    // SG
    "tcp://5.223.43.251:9651",
    // IND
    "tcp://142.93.217.194:9651",
    // DE QUIC
    "quic://188.40.132.242:9651",
    "quic://136.243.47.186:9651",
    // BE QUIC
    "quic://185.69.166.7:9651",
    "quic://185.69.166.8:9651",
    // FI QUIC
    "quic://65.21.231.58:9651",
    "quic://65.109.18.113:9651",
    // US-EAST QUIC
    "quic://209.159.146.190:9651",
    // US-WEST QUIC
    "quic://5.78.122.16:9651",
    // SG QUIC
    "quic://5.223.43.251:9651",
    // IND QUIC
    "quic://142.93.217.194:9651",
];

/// Node registry reference table (id, region, ipv4, ipv6, mycelium_ip).
pub const BOOTSTRAP_NODES: &[(&str, &str, &str, &str, &str)] = &[
    ("01", "DE",      "188.40.132.242",  "2a01:4f8:221:1e0b::2",              "54b:83ab:6cb5:7b38:44ae:cd14:53f3:a907"),
    ("02", "DE",      "136.243.47.186",  "2a01:4f8:212:fa6::2",               "40a:152c:b85b:9646:5b71:d03a:eb27:2462"),
    ("03", "BE",      "185.69.166.7",    "2a02:1802:5e:0:ec4:7aff:fe51:e80d", "597:a4ef:806:b09:6650:cbbf:1b68:cc94"),
    ("04", "BE",      "185.69.166.8",    "2a02:1802:5e:0:ec4:7aff:fe51:e36b", "549:8bce:fa45:e001:cbf8:f2e2:2da6:a67c"),
    ("05", "FI",      "65.21.231.58",    "2a01:4f9:6a:1dc5::2",               "410:2778:53bf:6f41:af28:1b60:d7c0:707a"),
    ("06", "FI",      "65.109.18.113",   "2a01:4f9:5a:1042::2",               "488:74ac:8a31:277b:9683:c8e:e14f:79a7"),
    ("07", "US-EAST", "209.159.146.190", "2604:a00:50:17b:9e6b:ff:fe1f:e054", "4ab:a385:5a4e:ef8f:92e0:1605:7cb6:24b2"),
    ("08", "US-WEST", "5.78.122.16",     "2a01:4ff:1f0:8859::1",              "4de:b695:3859:8234:d04c:5de6:8097:c27c"),
    ("09", "SG",      "5.223.43.251",    "2a01:4ff:2f0:3621::1",              "5eb:c711:f9ab:eb24:ff26:e392:a115:1c0e"),
    ("10", "IND",     "142.93.217.194",  "2400:6180:100:d0::841:2001",        "445:465:fe81:1e2b:5420:a029:6b0:9f61"),
];
