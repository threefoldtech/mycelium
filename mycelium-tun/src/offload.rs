//! Internal GSO/GRO offload logic.
//!
//! Handles splitting GRO-coalesced super-packets from the kernel into individual IP packets,
//! and coalescing multiple IP packets into GSO super-packets for the kernel.

use std::io;

use crate::checksum;

// ---------------------------------------------------------------------------
// virtio_net_hdr constants
// ---------------------------------------------------------------------------

pub const VIRTIO_NET_HDR_LEN: usize = 10;

// gso_type values
const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;
const VIRTIO_NET_HDR_GSO_TCPV4: u8 = 1;
const VIRTIO_NET_HDR_GSO_UDP_L4: u8 = 5;
const VIRTIO_NET_HDR_GSO_TCPV6: u8 = 4;

// flags
const VIRTIO_NET_HDR_F_NEEDS_CSUM: u8 = 1;

// IP protocol numbers
const IPPROTO_TCP: u8 = 6;
const IPPROTO_UDP: u8 = 17;

// IP header lengths
const IPV4_MIN_HEADER_LEN: usize = 20;
const IPV6_HEADER_LEN: usize = 40;
const TCP_MIN_HEADER_LEN: usize = 20;
const UDP_HEADER_LEN: usize = 8;

// IP versions
const IPV4_VERSION: u8 = 4;
const IPV6_VERSION: u8 = 6;

// TCP flags byte offset within TCP header
const TCP_FLAGS_OFFSET: usize = 13;
const TCP_FIN: u8 = 0x01;
const TCP_PSH: u8 = 0x08;

/// The 10-byte virtio_net_hdr prepended to every packet when IFF_VNET_HDR is set.
///
/// This matches the kernel's `struct virtio_net_hdr` (the default for TUN devices).
/// The 12-byte variant with `num_buffers` is only used in virtio device paths and would
/// require an explicit `TUNSETVNETHDRSZ` ioctl.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
}

impl VirtioNetHdr {
    /// Parse a VirtioNetHdr from the start of `raw`.
    pub fn decode(raw: &[u8]) -> io::Result<Self> {
        if raw.len() < VIRTIO_NET_HDR_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "buffer too small for virtio_net_hdr",
            ));
        }
        Ok(VirtioNetHdr {
            flags: raw[0],
            gso_type: raw[1],
            hdr_len: u16::from_le_bytes([raw[2], raw[3]]),
            gso_size: u16::from_le_bytes([raw[4], raw[5]]),
            csum_start: u16::from_le_bytes([raw[6], raw[7]]),
            csum_offset: u16::from_le_bytes([raw[8], raw[9]]),
        })
    }

    /// Encode to bytes, writing into `out`.
    pub fn encode(&self, out: &mut [u8]) {
        debug_assert!(out.len() >= VIRTIO_NET_HDR_LEN);
        out[0] = self.flags;
        out[1] = self.gso_type;
        out[2..4].copy_from_slice(&self.hdr_len.to_le_bytes());
        out[4..6].copy_from_slice(&self.gso_size.to_le_bytes());
        out[6..8].copy_from_slice(&self.csum_start.to_le_bytes());
        out[8..10].copy_from_slice(&self.csum_offset.to_le_bytes());
    }

    /// Create a GSO_NONE header for single-packet writes.
    pub fn none() -> Self {
        VirtioNetHdr::default()
    }
}

/// Information about a parsed IP + transport header.
struct HeaderInfo {
    ip_version: u8,
    ip_header_len: usize,
    protocol: u8,
    /// Total header length (IP + transport).
    total_header_len: usize,
}

/// Parse IP and transport headers from a packet.
fn parse_headers(pkt: &[u8]) -> io::Result<HeaderInfo> {
    if pkt.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty packet"));
    }

    let ip_version = pkt[0] >> 4;
    let (ip_header_len, protocol) = match ip_version {
        IPV4_VERSION => {
            if pkt.len() < IPV4_MIN_HEADER_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "packet too short for IPv4",
                ));
            }
            let ihl = (pkt[0] & 0x0f) as usize * 4;
            if ihl < IPV4_MIN_HEADER_LEN || pkt.len() < ihl {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid IPv4 IHL",
                ));
            }
            (ihl, pkt[9])
        }
        IPV6_VERSION => {
            if pkt.len() < IPV6_HEADER_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "packet too short for IPv6",
                ));
            }
            // next_header is the protocol; we don't handle extension headers for now.
            (IPV6_HEADER_LEN, pkt[6])
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported IP version {ip_version}"),
            ));
        }
    };

    let transport_header_len = match protocol {
        IPPROTO_TCP => {
            if pkt.len() < ip_header_len + TCP_MIN_HEADER_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "packet too short for TCP header",
                ));
            }
            let data_offset = ((pkt[ip_header_len + 12] >> 4) as usize) * 4;
            if data_offset < TCP_MIN_HEADER_LEN || pkt.len() < ip_header_len + data_offset {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid TCP data offset",
                ));
            }
            data_offset
        }
        IPPROTO_UDP => UDP_HEADER_LEN,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported transport protocol {protocol}"),
            ));
        }
    };

    let total_header_len = ip_header_len + transport_header_len;
    if pkt.len() < total_header_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "packet too short for full headers",
        ));
    }

    Ok(HeaderInfo {
        ip_version,
        ip_header_len,
        protocol,
        total_header_len,
    })
}

// ---------------------------------------------------------------------------
// GSO split — kernel → userspace (read path)
// ---------------------------------------------------------------------------

/// Split a raw kernel read buffer (virtio_net_hdr + payload) into individual IP packets.
///
/// Each resulting packet is written into `bufs[i]` and its length recorded in `sizes[i]`.
/// Returns the number of packets written.
pub fn gso_split(raw: &[u8], bufs: &mut [&mut [u8]], sizes: &mut [usize]) -> io::Result<usize> {
    if raw.len() < VIRTIO_NET_HDR_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "raw buffer too small for virtio header",
        ));
    }

    let hdr = VirtioNetHdr::decode(raw)?;
    let payload = &raw[VIRTIO_NET_HDR_LEN..];

    if hdr.gso_type == VIRTIO_NET_HDR_GSO_NONE {
        // Single packet — possibly needs checksum completion.
        if bufs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no output buffers",
            ));
        }
        if payload.len() > bufs[0].len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output buffer too small",
            ));
        }
        bufs[0][..payload.len()].copy_from_slice(payload);
        sizes[0] = payload.len();

        if hdr.flags & VIRTIO_NET_HDR_F_NEEDS_CSUM != 0 {
            complete_checksum(&mut bufs[0][..payload.len()], &hdr)?;
        }

        return Ok(1);
    }

    // GSO segmentation needed.
    let gso_size = hdr.gso_size as usize;
    if gso_size == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "gso_size is 0"));
    }

    let info = parse_headers(payload)?;
    let header_bytes = &payload[..info.total_header_len];
    let data = &payload[info.total_header_len..];

    if data.is_empty() {
        // No payload data to segment — just output the packet as-is.
        if bufs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no output buffers",
            ));
        }
        bufs[0][..payload.len()].copy_from_slice(payload);
        sizes[0] = payload.len();
        return Ok(1);
    }

    let is_tcp = info.protocol == IPPROTO_TCP;
    let mut offset = 0;
    let mut pkt_idx = 0;

    // For TCP: track sequence number increment.
    let base_seq = if is_tcp {
        u32::from_be_bytes([
            payload[info.ip_header_len + 4],
            payload[info.ip_header_len + 5],
            payload[info.ip_header_len + 6],
            payload[info.ip_header_len + 7],
        ])
    } else {
        0
    };

    // For IPv4: track IP ID increment.
    let base_ip_id = if info.ip_version == IPV4_VERSION {
        u16::from_be_bytes([payload[4], payload[5]])
    } else {
        0
    };

    while offset < data.len() {
        if pkt_idx >= bufs.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not enough output buffers for GSO segments",
            ));
        }

        let seg_len = gso_size.min(data.len() - offset);
        let total_len = info.total_header_len + seg_len;

        if total_len > bufs[pkt_idx].len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output buffer too small for segment",
            ));
        }

        let buf = &mut bufs[pkt_idx][..total_len];

        // Copy header template.
        buf[..info.total_header_len].copy_from_slice(header_bytes);
        // Copy segment payload.
        buf[info.total_header_len..].copy_from_slice(&data[offset..offset + seg_len]);

        let is_last = offset + seg_len >= data.len();

        // Fix up IP header.
        match info.ip_version {
            IPV4_VERSION => {
                // Total length.
                let ip_total = (total_len) as u16;
                buf[2..4].copy_from_slice(&ip_total.to_be_bytes());
                // IP identification.
                let ip_id = base_ip_id.wrapping_add(pkt_idx as u16);
                buf[4..6].copy_from_slice(&ip_id.to_be_bytes());
                // Recompute IPv4 header checksum.
                buf[10..12].copy_from_slice(&[0, 0]);
                let ip_csum = checksum::checksum(&buf[..info.ip_header_len], 0);
                buf[10..12].copy_from_slice(&ip_csum.to_be_bytes());
            }
            IPV6_VERSION => {
                // Payload length (excludes the 40-byte IPv6 header).
                let payload_len = (total_len - IPV6_HEADER_LEN) as u16;
                buf[4..6].copy_from_slice(&payload_len.to_be_bytes());
            }
            _ => unreachable!(),
        }

        // Fix up transport header.
        if is_tcp {
            // Sequence number.
            let seq = base_seq.wrapping_add(offset as u32);
            buf[info.ip_header_len + 4..info.ip_header_len + 8].copy_from_slice(&seq.to_be_bytes());

            // Clear FIN/PSH on non-last segments.
            if !is_last {
                buf[info.ip_header_len + TCP_FLAGS_OFFSET] &= !(TCP_FIN | TCP_PSH);
            }
        } else {
            // UDP: fix length field.
            let udp_len = (UDP_HEADER_LEN + seg_len) as u16;
            buf[info.ip_header_len + 4..info.ip_header_len + 6]
                .copy_from_slice(&udp_len.to_be_bytes());
        }

        // Compute transport checksum.
        compute_transport_checksum(buf, &info);

        sizes[pkt_idx] = total_len;
        pkt_idx += 1;
        offset += seg_len;
    }

    Ok(pkt_idx)
}

/// Complete a partial checksum as indicated by VIRTIO_NET_HDR_F_NEEDS_CSUM.
fn complete_checksum(pkt: &mut [u8], hdr: &VirtioNetHdr) -> io::Result<()> {
    let start = hdr.csum_start as usize;
    let offset = hdr.csum_offset as usize;

    if start + offset + 2 > pkt.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "checksum offset out of bounds",
        ));
    }

    // Zero the checksum field, compute over the region from csum_start to end.
    pkt[start + offset] = 0;
    pkt[start + offset + 1] = 0;

    // Build pseudo-header checksum.
    let info = parse_headers(pkt)?;
    let pseudo = pseudo_header_acc(pkt, &info);

    let csum = checksum::checksum(&pkt[start..], pseudo);
    pkt[start + offset..start + offset + 2].copy_from_slice(&csum.to_be_bytes());

    Ok(())
}

/// Compute and write the transport checksum for a fully formed packet.
fn compute_transport_checksum(pkt: &mut [u8], info: &HeaderInfo) {
    let csum_offset = match info.protocol {
        IPPROTO_TCP => info.ip_header_len + 16,
        IPPROTO_UDP => info.ip_header_len + 6,
        _ => return,
    };

    if csum_offset + 2 > pkt.len() {
        return;
    }

    // Zero the checksum field.
    pkt[csum_offset] = 0;
    pkt[csum_offset + 1] = 0;

    let pseudo = pseudo_header_acc(pkt, info);
    let csum = checksum::checksum(&pkt[info.ip_header_len..], pseudo);
    pkt[csum_offset..csum_offset + 2].copy_from_slice(&csum.to_be_bytes());
}

/// Compute the pseudo-header checksum accumulator for a packet.
fn pseudo_header_acc(pkt: &[u8], info: &HeaderInfo) -> u64 {
    let transport_len = pkt.len() - info.ip_header_len;
    match info.ip_version {
        IPV4_VERSION => checksum::pseudo_header_checksum_no_fold(
            info.protocol,
            &pkt[12..16],
            &pkt[16..20],
            transport_len as u16,
        ),
        IPV6_VERSION => checksum::pseudo_header_checksum_no_fold(
            info.protocol,
            &pkt[8..24],
            &pkt[24..40],
            transport_len as u16,
        ),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// GRO coalesce — userspace → kernel (write path)
// ---------------------------------------------------------------------------

/// Attempt to coalesce multiple packets into a single GSO super-packet.
///
/// Returns `(bytes_written, virtio_header)` on success. The coalesced payload is written to
/// `out` and the caller should prepend the returned virtio header.
///
/// If the packets cannot be coalesced (incompatible flows, single packet, etc.), returns
/// an error. The caller should then fall back to writing each packet individually with
/// a `GSO_NONE` header.
pub fn gro_coalesce(pkts: &[&[u8]], out: &mut [u8]) -> io::Result<(usize, VirtioNetHdr)> {
    if pkts.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "need at least 2 packets to coalesce",
        ));
    }

    let first = pkts[0];
    let info = parse_headers(first)?;

    // Validate all packets belong to the same flow and can be coalesced.
    let is_tcp = info.protocol == IPPROTO_TCP;

    // Extract flow key from first packet.
    let flow = FlowKey::from_packet(first, &info)?;

    // Collect segment data and validate contiguity.
    let first_payload_len = first.len() - info.total_header_len;
    if first_payload_len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "first packet has no payload",
        ));
    }

    // The GSO size is the payload length of the first (and all non-last) segments.
    let gso_size = first_payload_len;

    let mut expected_seq = if is_tcp {
        u32::from_be_bytes([
            first[info.ip_header_len + 4],
            first[info.ip_header_len + 5],
            first[info.ip_header_len + 6],
            first[info.ip_header_len + 7],
        ])
        .wrapping_add(first_payload_len as u32)
    } else {
        0
    };

    let mut expected_ip_id = if info.ip_version == IPV4_VERSION {
        u16::from_be_bytes([first[4], first[5]]).wrapping_add(1)
    } else {
        0
    };

    // Validate remaining packets.
    for &pkt in &pkts[1..] {
        let pkt_info = parse_headers(pkt)?;
        if pkt_info.protocol != info.protocol
            || pkt_info.ip_version != info.ip_version
            || pkt_info.total_header_len != info.total_header_len
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "incompatible packet headers",
            ));
        }

        let pkt_flow = FlowKey::from_packet(pkt, &pkt_info)?;
        if pkt_flow != flow {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "packets belong to different flows",
            ));
        }

        let payload_len = pkt.len() - info.total_header_len;

        if is_tcp {
            let seq = u32::from_be_bytes([
                pkt[info.ip_header_len + 4],
                pkt[info.ip_header_len + 5],
                pkt[info.ip_header_len + 6],
                pkt[info.ip_header_len + 7],
            ]);
            if seq != expected_seq {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "TCP sequence numbers not contiguous",
                ));
            }
            expected_seq = expected_seq.wrapping_add(payload_len as u32);

            // Non-last segments must have the same payload size as gso_size.
            // (Last segment can be smaller.)
        }

        // Validate IPv4 ID contiguity.
        if info.ip_version == IPV4_VERSION {
            let ip_id = u16::from_be_bytes([pkt[4], pkt[5]]);
            if ip_id != expected_ip_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "IPv4 IDs not contiguous",
                ));
            }
            expected_ip_id = expected_ip_id.wrapping_add(1);
        }
    }

    // All packets validated — build the coalesced buffer.
    // Layout: [first_packet_headers] [all_payloads_concatenated]
    let total_payload: usize = pkts.iter().map(|p| p.len() - info.total_header_len).sum();
    let total_len = info.total_header_len + total_payload;

    if total_len > out.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output buffer too small for coalesced packet",
        ));
    }

    // Copy headers from first packet.
    out[..info.total_header_len].copy_from_slice(&first[..info.total_header_len]);

    // Copy payloads.
    let mut pos = info.total_header_len;
    for &pkt in pkts {
        let payload = &pkt[info.total_header_len..];
        out[pos..pos + payload.len()].copy_from_slice(payload);
        pos += payload.len();
    }

    // Fix up IP total length / payload length.
    match info.ip_version {
        IPV4_VERSION => {
            out[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
            // Zero IPv4 header checksum — the kernel will handle it.
            out[10..12].copy_from_slice(&[0, 0]);
        }
        IPV6_VERSION => {
            let payload_len = (total_len - IPV6_HEADER_LEN) as u16;
            out[4..6].copy_from_slice(&payload_len.to_be_bytes());
        }
        _ => unreachable!(),
    }

    // Fix transport length for UDP.
    if !is_tcp {
        let udp_len = (UDP_HEADER_LEN + total_payload) as u16;
        out[info.ip_header_len + 4..info.ip_header_len + 6].copy_from_slice(&udp_len.to_be_bytes());
    }

    // Zero transport checksum — kernel computes it via VIRTIO_NET_HDR_F_NEEDS_CSUM.
    let csum_field_offset = if is_tcp {
        info.ip_header_len + 16
    } else {
        info.ip_header_len + 6
    };
    out[csum_field_offset..csum_field_offset + 2].copy_from_slice(&[0, 0]);

    // Build the virtio header.
    let gso_type = match (info.protocol, info.ip_version) {
        (IPPROTO_TCP, IPV4_VERSION) => VIRTIO_NET_HDR_GSO_TCPV4,
        (IPPROTO_TCP, IPV6_VERSION) => VIRTIO_NET_HDR_GSO_TCPV6,
        (IPPROTO_UDP, _) => VIRTIO_NET_HDR_GSO_UDP_L4,
        _ => VIRTIO_NET_HDR_GSO_NONE,
    };

    let vhdr = VirtioNetHdr {
        flags: VIRTIO_NET_HDR_F_NEEDS_CSUM,
        gso_type,
        hdr_len: info.total_header_len as u16,
        gso_size: gso_size as u16,
        csum_start: info.ip_header_len as u16,
        csum_offset: if is_tcp { 16 } else { 6 },
    };

    Ok((total_len, vhdr))
}

/// Flow key for identifying compatible packets.
#[derive(Debug, PartialEq, Eq)]
struct FlowKey {
    src_addr: [u8; 16],
    dst_addr: [u8; 16],
    src_port: u16,
    dst_port: u16,
    protocol: u8,
}

impl FlowKey {
    fn from_packet(pkt: &[u8], info: &HeaderInfo) -> io::Result<Self> {
        let (src_addr, dst_addr) = match info.ip_version {
            IPV4_VERSION => {
                let mut src = [0u8; 16];
                let mut dst = [0u8; 16];
                src[..4].copy_from_slice(&pkt[12..16]);
                dst[..4].copy_from_slice(&pkt[16..20]);
                (src, dst)
            }
            IPV6_VERSION => {
                let mut src = [0u8; 16];
                let mut dst = [0u8; 16];
                src.copy_from_slice(&pkt[8..24]);
                dst.copy_from_slice(&pkt[24..40]);
                (src, dst)
            }
            _ => unreachable!(),
        };

        let transport_offset = info.ip_header_len;
        let src_port = u16::from_be_bytes([pkt[transport_offset], pkt[transport_offset + 1]]);
        let dst_port = u16::from_be_bytes([pkt[transport_offset + 2], pkt[transport_offset + 3]]);

        Ok(FlowKey {
            src_addr,
            dst_addr,
            src_port,
            dst_port,
            protocol: info.protocol,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal IPv4/TCP packet.
    fn make_ipv4_tcp_packet(
        src: [u8; 4],
        dst: [u8; 4],
        src_port: u16,
        dst_port: u16,
        seq: u32,
        ip_id: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let total_len = IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN + payload.len();
        let mut pkt = vec![0u8; total_len];

        // IPv4 header.
        pkt[0] = 0x45; // version=4, IHL=5
        pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        pkt[4..6].copy_from_slice(&ip_id.to_be_bytes());
        pkt[8] = 64; // TTL
        pkt[9] = IPPROTO_TCP;
        pkt[12..16].copy_from_slice(&src);
        pkt[16..20].copy_from_slice(&dst);

        // IPv4 header checksum.
        let csum = checksum::checksum(&pkt[..IPV4_MIN_HEADER_LEN], 0);
        pkt[10..12].copy_from_slice(&csum.to_be_bytes());

        // TCP header.
        let tcp = IPV4_MIN_HEADER_LEN;
        pkt[tcp..tcp + 2].copy_from_slice(&src_port.to_be_bytes());
        pkt[tcp + 2..tcp + 4].copy_from_slice(&dst_port.to_be_bytes());
        pkt[tcp + 4..tcp + 8].copy_from_slice(&seq.to_be_bytes());
        pkt[tcp + 12] = 0x50; // data offset = 5 (20 bytes)
        pkt[tcp + 13] = 0x10; // ACK flag

        // Payload.
        pkt[tcp + TCP_MIN_HEADER_LEN..].copy_from_slice(payload);

        // TCP checksum.
        let info = parse_headers(&pkt).unwrap();
        compute_transport_checksum(&mut pkt, &info);

        pkt
    }

    /// Build a minimal IPv4/UDP packet.
    fn make_ipv4_udp_packet(
        src: [u8; 4],
        dst: [u8; 4],
        src_port: u16,
        dst_port: u16,
        ip_id: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let total_len = IPV4_MIN_HEADER_LEN + UDP_HEADER_LEN + payload.len();
        let mut pkt = vec![0u8; total_len];

        // IPv4 header.
        pkt[0] = 0x45;
        pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        pkt[4..6].copy_from_slice(&ip_id.to_be_bytes());
        pkt[8] = 64;
        pkt[9] = IPPROTO_UDP;
        pkt[12..16].copy_from_slice(&src);
        pkt[16..20].copy_from_slice(&dst);

        let csum = checksum::checksum(&pkt[..IPV4_MIN_HEADER_LEN], 0);
        pkt[10..12].copy_from_slice(&csum.to_be_bytes());

        // UDP header.
        let udp = IPV4_MIN_HEADER_LEN;
        pkt[udp..udp + 2].copy_from_slice(&src_port.to_be_bytes());
        pkt[udp + 2..udp + 4].copy_from_slice(&dst_port.to_be_bytes());
        let udp_len = (UDP_HEADER_LEN + payload.len()) as u16;
        pkt[udp + 4..udp + 6].copy_from_slice(&udp_len.to_be_bytes());

        // Payload.
        pkt[udp + UDP_HEADER_LEN..].copy_from_slice(payload);

        // UDP checksum.
        let info = parse_headers(&pkt).unwrap();
        compute_transport_checksum(&mut pkt, &info);

        pkt
    }

    #[test]
    fn test_gso_split_gso_none() {
        let pkt = make_ipv4_tcp_packet([10, 0, 0, 1], [10, 0, 0, 2], 1234, 80, 1000, 1, b"hello");

        // Build raw with GSO_NONE header.
        let mut raw = vec![0u8; VIRTIO_NET_HDR_LEN + pkt.len()];
        VirtioNetHdr::none().encode(&mut raw[..VIRTIO_NET_HDR_LEN]);
        raw[VIRTIO_NET_HDR_LEN..].copy_from_slice(&pkt);

        let mut buf = vec![0u8; 1500];
        let mut bufs: Vec<&mut [u8]> = vec![buf.as_mut_slice()];
        let mut sizes = vec![0usize; 1];

        let n = gso_split(&raw, &mut bufs, &mut sizes).unwrap();
        assert_eq!(n, 1);
        assert_eq!(sizes[0], pkt.len());
        assert_eq!(&bufs[0][..sizes[0]], &pkt[..]);
    }

    #[test]
    fn test_gso_split_tcp() {
        // Build a coalesced TCP packet: 2 segments of 10 bytes each.
        let seg_size = 10;
        let payload = vec![0xAA; seg_size * 2];
        let total_header = IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN;
        let total_len = total_header + payload.len();

        let mut super_pkt = vec![0u8; total_len];
        super_pkt[0] = 0x45;
        super_pkt[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
        super_pkt[4..6].copy_from_slice(&1u16.to_be_bytes()); // IP ID
        super_pkt[8] = 64;
        super_pkt[9] = IPPROTO_TCP;
        super_pkt[12..16].copy_from_slice(&[10, 0, 0, 1]);
        super_pkt[16..20].copy_from_slice(&[10, 0, 0, 2]);

        let tcp = IPV4_MIN_HEADER_LEN;
        super_pkt[tcp..tcp + 2].copy_from_slice(&1234u16.to_be_bytes());
        super_pkt[tcp + 2..tcp + 4].copy_from_slice(&80u16.to_be_bytes());
        super_pkt[tcp + 4..tcp + 8].copy_from_slice(&1000u32.to_be_bytes());
        super_pkt[tcp + 12] = 0x50; // data offset = 5
        super_pkt[tcp + 13] = 0x19; // ACK + FIN + PSH

        super_pkt[total_header..].copy_from_slice(&payload);

        // Build raw buffer with virtio header.
        let mut raw = vec![0u8; VIRTIO_NET_HDR_LEN + total_len];
        let vhdr = VirtioNetHdr {
            flags: 0,
            gso_type: VIRTIO_NET_HDR_GSO_TCPV4,
            hdr_len: total_header as u16,
            gso_size: seg_size as u16,
            csum_start: IPV4_MIN_HEADER_LEN as u16,
            csum_offset: 16,
        };
        vhdr.encode(&mut raw[..VIRTIO_NET_HDR_LEN]);
        raw[VIRTIO_NET_HDR_LEN..].copy_from_slice(&super_pkt);

        let mut buf0 = vec![0u8; 1500];
        let mut buf1 = vec![0u8; 1500];
        let mut bufs: Vec<&mut [u8]> = vec![buf0.as_mut_slice(), buf1.as_mut_slice()];
        let mut sizes = vec![0usize; 2];

        let n = gso_split(&raw, &mut bufs, &mut sizes).unwrap();
        assert_eq!(n, 2);

        // First segment.
        assert_eq!(sizes[0], total_header + seg_size);
        // Check sequence number is 1000.
        let seg0_seq = u32::from_be_bytes([
            bufs[0][tcp + 4],
            bufs[0][tcp + 5],
            bufs[0][tcp + 6],
            bufs[0][tcp + 7],
        ]);
        assert_eq!(seg0_seq, 1000);
        // FIN/PSH should be cleared on first segment.
        assert_eq!(bufs[0][tcp + 13] & (TCP_FIN | TCP_PSH), 0);

        // Second segment.
        assert_eq!(sizes[1], total_header + seg_size);
        let seg1_seq = u32::from_be_bytes([
            bufs[1][tcp + 4],
            bufs[1][tcp + 5],
            bufs[1][tcp + 6],
            bufs[1][tcp + 7],
        ]);
        assert_eq!(seg1_seq, 1010);
        // FIN/PSH preserved on last segment.
        assert_eq!(bufs[1][tcp + 13] & (TCP_FIN | TCP_PSH), TCP_FIN | TCP_PSH);

        // IP IDs should be sequential.
        let id0 = u16::from_be_bytes([bufs[0][4], bufs[0][5]]);
        let id1 = u16::from_be_bytes([bufs[1][4], bufs[1][5]]);
        assert_eq!(id0, 1);
        assert_eq!(id1, 2);

        // Verify IPv4 header checksums are valid.
        assert_eq!(checksum::checksum(&bufs[0][..IPV4_MIN_HEADER_LEN], 0), 0);
        assert_eq!(checksum::checksum(&bufs[1][..IPV4_MIN_HEADER_LEN], 0), 0);
    }

    #[test]
    fn test_gro_coalesce_tcp() {
        let pkt0 = make_ipv4_tcp_packet(
            [10, 0, 0, 1],
            [10, 0, 0, 2],
            1234,
            80,
            1000,
            1,
            &[0xAA; 100],
        );
        let pkt1 = make_ipv4_tcp_packet(
            [10, 0, 0, 1],
            [10, 0, 0, 2],
            1234,
            80,
            1100,
            2,
            &[0xBB; 100],
        );

        let pkts: Vec<&[u8]> = vec![&pkt0, &pkt1];
        let mut out = vec![0u8; 65536];
        let (len, vhdr) = gro_coalesce(&pkts, &mut out).unwrap();

        assert_eq!(vhdr.gso_type, VIRTIO_NET_HDR_GSO_TCPV4);
        assert_eq!(vhdr.gso_size, 100);
        assert_eq!(len, IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN + 200);
        assert_eq!(vhdr.flags, VIRTIO_NET_HDR_F_NEEDS_CSUM);
    }

    #[test]
    fn test_gro_coalesce_different_flows_fails() {
        let pkt0 = make_ipv4_tcp_packet(
            [10, 0, 0, 1],
            [10, 0, 0, 2],
            1234,
            80,
            1000,
            1,
            &[0xAA; 100],
        );
        // Different destination port.
        let pkt1 = make_ipv4_tcp_packet(
            [10, 0, 0, 1],
            [10, 0, 0, 2],
            1234,
            81,
            1100,
            2,
            &[0xBB; 100],
        );

        let pkts: Vec<&[u8]> = vec![&pkt0, &pkt1];
        let mut out = vec![0u8; 65536];
        assert!(gro_coalesce(&pkts, &mut out).is_err());
    }

    #[test]
    fn test_gro_coalesce_non_contiguous_seq_fails() {
        let pkt0 = make_ipv4_tcp_packet(
            [10, 0, 0, 1],
            [10, 0, 0, 2],
            1234,
            80,
            1000,
            1,
            &[0xAA; 100],
        );
        // Gap in sequence numbers.
        let pkt1 = make_ipv4_tcp_packet(
            [10, 0, 0, 1],
            [10, 0, 0, 2],
            1234,
            80,
            1200,
            2,
            &[0xBB; 100],
        );

        let pkts: Vec<&[u8]> = vec![&pkt0, &pkt1];
        let mut out = vec![0u8; 65536];
        assert!(gro_coalesce(&pkts, &mut out).is_err());
    }

    #[test]
    fn test_roundtrip_split_coalesce() {
        // Create 3 TCP segments, coalesce them, then split.
        let seg_size = 50;
        let pkt0 = make_ipv4_tcp_packet(
            [192, 168, 1, 1],
            [192, 168, 1, 2],
            5000,
            443,
            0,
            10,
            &vec![1u8; seg_size],
        );
        let pkt1 = make_ipv4_tcp_packet(
            [192, 168, 1, 1],
            [192, 168, 1, 2],
            5000,
            443,
            50,
            11,
            &vec![2u8; seg_size],
        );
        let pkt2 = make_ipv4_tcp_packet(
            [192, 168, 1, 1],
            [192, 168, 1, 2],
            5000,
            443,
            100,
            12,
            &vec![3u8; seg_size],
        );

        // Coalesce.
        let pkts: Vec<&[u8]> = vec![&pkt0, &pkt1, &pkt2];
        let mut coalesced = vec![0u8; 65536];
        let (len, vhdr) = gro_coalesce(&pkts, &mut coalesced).unwrap();

        // Build raw buffer as if read from kernel.
        let mut raw = vec![0u8; VIRTIO_NET_HDR_LEN + len];
        vhdr.encode(&mut raw[..VIRTIO_NET_HDR_LEN]);
        raw[VIRTIO_NET_HDR_LEN..VIRTIO_NET_HDR_LEN + len].copy_from_slice(&coalesced[..len]);

        // Split.
        let mut buf0 = vec![0u8; 1500];
        let mut buf1 = vec![0u8; 1500];
        let mut buf2 = vec![0u8; 1500];
        let mut bufs: Vec<&mut [u8]> = vec![
            buf0.as_mut_slice(),
            buf1.as_mut_slice(),
            buf2.as_mut_slice(),
        ];
        let mut sizes = vec![0usize; 3];

        let n = gso_split(&raw, &mut bufs, &mut sizes).unwrap();
        assert_eq!(n, 3);

        // Verify payloads match original.
        let hdr_len = IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN;
        assert_eq!(&bufs[0][hdr_len..sizes[0]], &vec![1u8; seg_size][..]);
        assert_eq!(&bufs[1][hdr_len..sizes[1]], &vec![2u8; seg_size][..]);
        assert_eq!(&bufs[2][hdr_len..sizes[2]], &vec![3u8; seg_size][..]);
    }

    #[test]
    fn test_gso_split_needs_csum() {
        // Test NEEDS_CSUM flag with GSO_NONE — should compute checksum.
        let pkt = make_ipv4_tcp_packet([10, 0, 0, 1], [10, 0, 0, 2], 1234, 80, 1000, 1, b"test");

        // Zero out TCP checksum and set NEEDS_CSUM.
        let mut raw = vec![0u8; VIRTIO_NET_HDR_LEN + pkt.len()];
        let mut modified_pkt = pkt.clone();
        modified_pkt[IPV4_MIN_HEADER_LEN + 16] = 0;
        modified_pkt[IPV4_MIN_HEADER_LEN + 17] = 0;

        let vhdr = VirtioNetHdr {
            flags: VIRTIO_NET_HDR_F_NEEDS_CSUM,
            gso_type: VIRTIO_NET_HDR_GSO_NONE,
            hdr_len: (IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN) as u16,
            gso_size: 0,
            csum_start: IPV4_MIN_HEADER_LEN as u16,
            csum_offset: 16,
        };
        vhdr.encode(&mut raw[..VIRTIO_NET_HDR_LEN]);
        raw[VIRTIO_NET_HDR_LEN..].copy_from_slice(&modified_pkt);

        let mut buf = vec![0u8; 1500];
        let mut bufs: Vec<&mut [u8]> = vec![buf.as_mut_slice()];
        let mut sizes = vec![0usize; 1];

        let n = gso_split(&raw, &mut bufs, &mut sizes).unwrap();
        assert_eq!(n, 1);

        // TCP checksum should now be valid (same as original).
        let tcp_csum = u16::from_be_bytes([
            bufs[0][IPV4_MIN_HEADER_LEN + 16],
            bufs[0][IPV4_MIN_HEADER_LEN + 17],
        ]);
        let orig_csum =
            u16::from_be_bytes([pkt[IPV4_MIN_HEADER_LEN + 16], pkt[IPV4_MIN_HEADER_LEN + 17]]);
        assert_eq!(tcp_csum, orig_csum);
    }

    #[test]
    fn test_gro_coalesce_udp() {
        let pkt0 = make_ipv4_udp_packet([10, 0, 0, 1], [10, 0, 0, 2], 5000, 53, 1, &[0xAA; 100]);
        let pkt1 = make_ipv4_udp_packet([10, 0, 0, 1], [10, 0, 0, 2], 5000, 53, 2, &[0xBB; 100]);

        let pkts: Vec<&[u8]> = vec![&pkt0, &pkt1];
        let mut out = vec![0u8; 65536];
        let (len, vhdr) = gro_coalesce(&pkts, &mut out).unwrap();

        assert_eq!(vhdr.gso_type, VIRTIO_NET_HDR_GSO_UDP_L4);
        assert_eq!(vhdr.gso_size, 100);
        assert_eq!(len, IPV4_MIN_HEADER_LEN + UDP_HEADER_LEN + 200);
    }

    #[test]
    fn test_virtio_hdr_encode_decode_roundtrip() {
        let hdr = VirtioNetHdr {
            flags: VIRTIO_NET_HDR_F_NEEDS_CSUM,
            gso_type: VIRTIO_NET_HDR_GSO_TCPV4,
            hdr_len: 54,
            gso_size: 1460,
            csum_start: 34,
            csum_offset: 16,
        };

        let mut buf = [0u8; VIRTIO_NET_HDR_LEN];
        hdr.encode(&mut buf);
        let decoded = VirtioNetHdr::decode(&buf).unwrap();

        assert_eq!(decoded.flags, hdr.flags);
        assert_eq!(decoded.gso_type, hdr.gso_type);
        assert_eq!(decoded.hdr_len, hdr.hdr_len);
        assert_eq!(decoded.gso_size, hdr.gso_size);
        assert_eq!(decoded.csum_start, hdr.csum_start);
        assert_eq!(decoded.csum_offset, hdr.csum_offset);
    }
}
