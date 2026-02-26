//! Internet checksum (RFC 1071) with optional SIMD acceleration.

/// Accumulate a ones-complement checksum over `data` without folding.
///
/// The `initial` value allows chaining multiple regions. The result is a 64-bit accumulator
/// that must be folded with [`checksum_fold`] to produce the final 16-bit checksum.
pub fn checksum_no_fold(data: &[u8], initial: u64) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: we checked for AVX2 support above.
            return unsafe { checksum_no_fold_avx2(data, initial) };
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            // SAFETY: we checked for NEON support above.
            return unsafe { checksum_no_fold_neon(data, initial) };
        }
    }

    checksum_no_fold_portable(data, initial)
}

/// Fold a 64-bit accumulator into a 16-bit ones-complement checksum.
pub fn checksum_fold(mut acc: u64) -> u16 {
    acc = (acc >> 32) + (acc & 0xffff_ffff);
    acc = (acc >> 16) + (acc & 0xffff);
    acc = (acc >> 16) + (acc & 0xffff);
    acc = (acc >> 16) + (acc & 0xffff);
    !(acc as u16)
}

/// Compute the ones-complement checksum over `data`.
pub fn checksum(data: &[u8], initial: u64) -> u16 {
    checksum_fold(checksum_no_fold(data, initial))
}

/// Compute the pseudo-header checksum accumulator for TCP/UDP over IPv4 or IPv6.
///
/// `src` and `dst` must be 4 bytes (IPv4) or 16 bytes (IPv6).
pub fn pseudo_header_checksum_no_fold(proto: u8, src: &[u8], dst: &[u8], total_len: u16) -> u64 {
    let mut acc = checksum_no_fold(src, 0);
    acc = checksum_no_fold(dst, acc);

    // Protocol and length in network byte order, padded as a 4-byte chunk:
    // [0, proto, len_hi, len_lo]
    let buf = [0u8, proto, (total_len >> 8) as u8, total_len as u8];
    checksum_no_fold(&buf, acc)
}

// ---------------------------------------------------------------------------
// Portable implementation
// ---------------------------------------------------------------------------

fn checksum_no_fold_portable(data: &[u8], initial: u64) -> u64 {
    let mut acc = initial;
    let mut i = 0;

    // Process 8 bytes at a time.
    while i + 8 <= data.len() {
        let word = u64::from_be_bytes([
            data[i],
            data[i + 1],
            data[i + 2],
            data[i + 3],
            data[i + 4],
            data[i + 5],
            data[i + 6],
            data[i + 7],
        ]);
        let (sum, carry) = acc.overflowing_add(word);
        acc = sum + carry as u64;
        i += 8;
    }

    // Process 4-byte remainder.
    if i + 4 <= data.len() {
        let word = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as u64;
        let (sum, carry) = acc.overflowing_add(word << 32);
        acc = sum + carry as u64;
        i += 4;
    }

    // Process 2-byte remainder.
    if i + 2 <= data.len() {
        let word = u16::from_be_bytes([data[i], data[i + 1]]) as u64;
        let (sum, carry) = acc.overflowing_add(word << 48);
        acc = sum + carry as u64;
        i += 2;
    }

    // Process final byte (padded to 16-bit boundary).
    if i < data.len() {
        let word = (data[i] as u64) << 56;
        let (sum, carry) = acc.overflowing_add(word);
        acc = sum + carry as u64;
    }

    acc
}

// ---------------------------------------------------------------------------
// x86_64 AVX2 implementation
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn checksum_no_fold_avx2(data: &[u8], initial: u64) -> u64 {
    use std::arch::x86_64::*;

    let mut acc = initial;
    let mut i = 0;

    // SAFETY: all intrinsics below require AVX2 which is guaranteed by #[target_feature].
    unsafe {
        // Byte-swap mask: reverses bytes within each 64-bit lane (big-endian to little-endian).
        let bswap_mask = _mm256_set_epi8(
            8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0,
            1, 2, 3, 4, 5, 6, 7,
        );

        // Process 32 bytes per iteration: SIMD byte-swap to native-endian u64 lanes,
        // then extract and accumulate with ones-complement carry into scalar accumulator.
        while i + 32 <= data.len() {
            let chunk = _mm256_loadu_si256(data.as_ptr().add(i) as *const __m256i);
            let swapped = _mm256_shuffle_epi8(chunk, bswap_mask);

            let lane0 = _mm256_extract_epi64::<0>(swapped) as u64;
            let lane1 = _mm256_extract_epi64::<1>(swapped) as u64;
            let lane2 = _mm256_extract_epi64::<2>(swapped) as u64;
            let lane3 = _mm256_extract_epi64::<3>(swapped) as u64;

            let (sum, c) = acc.overflowing_add(lane0);
            acc = sum + c as u64;
            let (sum, c) = acc.overflowing_add(lane1);
            acc = sum + c as u64;
            let (sum, c) = acc.overflowing_add(lane2);
            acc = sum + c as u64;
            let (sum, c) = acc.overflowing_add(lane3);
            acc = sum + c as u64;

            i += 32;
        }
    }

    // Handle remainder with portable path.
    checksum_no_fold_portable(&data[i..], acc)
}

// ---------------------------------------------------------------------------
// aarch64 NEON implementation
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn checksum_no_fold_neon(data: &[u8], initial: u64) -> u64 {
    use std::arch::aarch64::*;

    let mut acc = initial;
    let mut i = 0;

    // SAFETY: all intrinsics below require NEON which is guaranteed by #[target_feature].
    unsafe {
        // Process 16 bytes (2 x u64) at a time. Extract lanes each iteration for correct carry.
        while i + 16 <= data.len() {
            let chunk = vld1q_u8(data.as_ptr().add(i));
            let reversed = vrev64q_u8(chunk);
            let words: uint64x2_t = vreinterpretq_u64_u8(reversed);

            let lane0 = vgetq_lane_u64::<0>(words);
            let lane1 = vgetq_lane_u64::<1>(words);
            let (sum, c) = acc.overflowing_add(lane0);
            acc = sum + c as u64;
            let (sum, c) = acc.overflowing_add(lane1);
            acc = sum + c as u64;

            i += 16;
        }
    }

    // Handle remainder with portable path.
    checksum_no_fold_portable(&data[i..], acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rfc1071_example() {
        // RFC 1071 example: 0x0001 + 0xf203 + 0xf4f5 + 0xf6f7 = 0xddf2 (complement = 0x220d)
        let data = [0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7];
        assert_eq!(checksum(&data, 0), 0x220d);
    }

    #[test]
    fn test_empty() {
        assert_eq!(checksum(&[], 0), 0xffff);
    }

    #[test]
    fn test_single_byte() {
        // 0x42 padded to 0x4200 → complement = 0xbdff
        assert_eq!(checksum(&[0x42], 0), 0xbdff);
    }

    #[test]
    fn test_checksum_fold_identity() {
        // All zeros should fold to 0xffff (complement of 0).
        assert_eq!(checksum_fold(0), 0xffff);
    }

    #[test]
    fn test_pseudo_header_ipv6_tcp() {
        // IPv6 pseudo-header for TCP (proto 6).
        let src = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        let dst = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
        let acc = pseudo_header_checksum_no_fold(6, &src, &dst, 20);
        // Should produce a non-zero accumulator.
        assert_ne!(acc, 0);
    }

    #[test]
    fn test_portable_matches_known() {
        // A known IPv4 header checksum test.
        // IPv4 header: version=4, IHL=5, total_len=40, id=0, flags=0, ttl=64, proto=6(TCP)
        // src=192.168.1.1, dst=192.168.1.2, checksum field zeroed.
        let header = [
            0x45, 0x00, 0x00, 0x28, // version, IHL, DSCP, total length
            0x00, 0x00, 0x00, 0x00, // identification, flags, fragment offset
            0x40, 0x06, 0x00, 0x00, // TTL, protocol, checksum (zeroed)
            0xc0, 0xa8, 0x01, 0x01, // source IP
            0xc0, 0xa8, 0x01, 0x02, // dest IP
        ];
        let csum = checksum(&header, 0);
        // Verify the header with the computed checksum validates to 0.
        let mut with_csum = header;
        with_csum[10] = (csum >> 8) as u8;
        with_csum[11] = csum as u8;
        assert_eq!(checksum(&with_csum, 0), 0);
    }

    #[test]
    fn test_large_data() {
        // Test with data larger than 32 bytes to exercise SIMD paths.
        let data: Vec<u8> = (0..256).map(|i| i as u8).collect();
        let expected = checksum_no_fold_portable(&data, 0);
        let actual = checksum_no_fold(&data, 0);
        assert_eq!(checksum_fold(actual), checksum_fold(expected));
    }

    #[test]
    fn test_chaining() {
        // Checksum of [A, B] should equal chaining checksum of A then B.
        let a = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let b = [0x09, 0x0a, 0x0b, 0x0c];
        let combined: Vec<u8> = a.iter().chain(b.iter()).copied().collect();

        let full = checksum(&combined, 0);
        let chained = checksum_fold(checksum_no_fold(&b, checksum_no_fold(&a, 0)));
        assert_eq!(full, chained);
    }
}
