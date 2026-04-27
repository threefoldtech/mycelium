//! `#[repr(C)]` data types exposed across the C FFI boundary, plus the
//! per-type free functions callers must use to release any heap memory the
//! library handed back.

#![allow(non_camel_case_types)]

use core::ffi::c_char;
use std::ffi::CString;
use std::sync::Mutex;

use crate::node_handle::NodeHandle;

/// Opaque handle to a running (or stopped) mycelium node.
///
/// Created by [`mycelium_start`](super::mycelium_start), released by
/// [`mycelium_node_free`](super::mycelium_node_free). The C side only ever
/// sees an opaque pointer — cbindgen emits a forward declaration for this
/// struct because it is not `#[repr(C)]`.
pub struct mycelium_node_t {
    pub(super) state: Mutex<NodeState>,
}

pub(super) enum NodeState {
    Running(NodeHandle),
    Stopped,
}

/// Configuration passed to `mycelium_start`. The library reads strings out
/// during the call and does not retain any of the pointers — caller still
/// owns the memory.
#[repr(C)]
pub struct mycelium_start_config_t {
    /// 32-byte node secret key.
    pub priv_key: [u8; 32],
    /// Array of bootstrap peer endpoints (NUL-terminated UTF-8).
    pub peers: *const *const c_char,
    /// Number of entries in `peers`.
    pub peers_len: usize,
    /// Enable the embedded DNS resolver.
    pub enable_dns: bool,
    /// TCP listen port.
    pub tcp_listen_port: u16,
    /// QUIC listen port; 0 disables QUIC.
    pub quic_listen_port: u16,
    /// UDP port for peer discovery.
    pub peer_discovery_port: u16,
    /// Discovery mode: "all", "disabled", or "filtered".
    pub peer_discovery_mode: *const c_char,
    /// When mode is "filtered", the allow-list of interface names.
    pub peer_discovery_interfaces: *const *const c_char,
    /// Number of entries in `peer_discovery_interfaces`.
    pub peer_discovery_interfaces_len: usize,
    /// Name of the TUN device.
    pub tun_name: *const c_char,
    /// Pre-opened TUN file descriptor. Required on iOS and on macOS when
    /// the `mactunfd` feature is enabled (the platform delivers the fd
    /// through Network Extensions / app-store sandbox plumbing). Ignored
    /// on Linux, Windows, plain macOS, and Android — Android opens
    /// `/dev/tun` itself from `tun_name`.
    pub tun_fd: i32,
}

/// 32-byte secret key (output of `mycelium_generate_secret_key`).
#[repr(C)]
pub struct mycelium_secret_key_t {
    pub bytes: [u8; 32],
}

/// Public node identity.
#[repr(C)]
pub struct mycelium_node_info_t {
    pub subnet: *mut c_char,
    pub pubkey: *mut c_char,
}

/// Information about a known peer connection.
#[repr(C)]
pub struct mycelium_peer_info_t {
    pub protocol: *mut c_char,
    pub address: *mut c_char,
    pub peer_type: *mut c_char,
    pub connection_state: *mut c_char,
    pub rx_bytes: i64,
    pub tx_bytes: i64,
    pub discovered_seconds: i64,
    /// `-1` if the peer was never connected.
    pub last_connected_seconds: i64,
}

/// A route entry (selected or fallback).
#[repr(C)]
pub struct mycelium_route_t {
    pub subnet: *mut c_char,
    pub next_hop: *mut c_char,
    /// `-1` means infinite metric.
    pub metric: i64,
    pub seqno: i32,
}

/// A subnet currently being queried by the routing layer.
#[repr(C)]
pub struct mycelium_queried_subnet_t {
    pub subnet: *mut c_char,
    pub expiration_seconds: i64,
}

/// Per-IP packet counter entry.
#[repr(C)]
pub struct mycelium_packet_stat_entry_t {
    pub ip: *mut c_char,
    pub packet_count: i64,
    pub byte_count: i64,
}

/// Aggregated packet statistics.
#[repr(C)]
pub struct mycelium_packet_stats_t {
    pub by_source: *mut mycelium_packet_stat_entry_t,
    pub by_source_len: usize,
    pub by_destination: *mut mycelium_packet_stat_entry_t,
    pub by_destination_len: usize,
}

#[repr(C)]
pub struct mycelium_peer_info_array_t {
    pub items: *mut mycelium_peer_info_t,
    pub len: usize,
}

#[repr(C)]
pub struct mycelium_route_array_t {
    pub items: *mut mycelium_route_t,
    pub len: usize,
}

#[repr(C)]
pub struct mycelium_queried_subnet_array_t {
    pub items: *mut mycelium_queried_subnet_t,
    pub len: usize,
}

#[repr(C)]
pub struct mycelium_string_array_t {
    pub items: *mut *mut c_char,
    pub len: usize,
}

// ---------------------------------------------------------------------------
// Allocation helpers used by service.rs to hand owned data to the caller.
// ---------------------------------------------------------------------------

pub(super) fn cstring(s: impl Into<Vec<u8>>) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

pub(super) fn vec_into_c<T>(v: Vec<T>) -> (*mut T, usize) {
    let len = v.len();
    if len == 0 {
        return (std::ptr::null_mut(), 0);
    }
    let boxed = v.into_boxed_slice();
    let ptr = Box::into_raw(boxed) as *mut T;
    (ptr, len)
}

pub(super) unsafe fn free_cstring(ptr: *mut c_char) {
    if !ptr.is_null() {
        let _ = CString::from_raw(ptr);
    }
}

pub(super) unsafe fn drain_array<T>(ptr: *mut T, len: usize) -> Vec<T> {
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    let slice = std::ptr::slice_from_raw_parts_mut(ptr, len);
    Box::from_raw(slice).into_vec()
}

// ---------------------------------------------------------------------------
// Per-type destructors.
// ---------------------------------------------------------------------------

/// Release a node handle returned by `mycelium_start`. If the node is still
/// running it is stopped first. Safe to call with NULL.
///
/// # Safety
///
/// `node` must be NULL or a pointer previously returned by `mycelium_start`
/// that has not already been passed to this function. Double-free or
/// use-after-free is undefined behaviour.
#[no_mangle]
pub unsafe extern "C" fn mycelium_node_free(node: *mut mycelium_node_t) {
    if node.is_null() {
        return;
    }
    let _ = Box::from_raw(node);
}

/// Free a single C string returned by the library. Safe to call with NULL.
///
/// # Safety
///
/// `ptr` must be NULL or a value returned by a `mycelium_*` entry point
/// that hands the caller ownership of a `char *` (e.g.
/// `mycelium_get_public_key_from_ip`, `mycelium_address_from_secret_key`,
/// `mycelium_proxy_connect`). Strings retrieved via `mycelium_last_error_message`
/// are owned by the library and must NOT be passed here.
#[no_mangle]
pub unsafe extern "C" fn mycelium_string_free(ptr: *mut c_char) {
    free_cstring(ptr);
}

/// Free a string array returned by the library. The outer struct is
/// caller-allocated; this releases the items and the backing storage, then
/// resets `items` to NULL and `len` to 0.
///
/// # Safety
///
/// `arr` must be NULL or point to a `mycelium_string_array_t` populated by
/// a successful library call (currently only `mycelium_list_proxies`).
/// Calling this twice on the same struct is safe — the second call sees
/// `items == NULL` and does nothing.
#[no_mangle]
pub unsafe extern "C" fn mycelium_string_array_free(arr: *mut mycelium_string_array_t) {
    if arr.is_null() {
        return;
    }
    let arr = &mut *arr;
    for item in drain_array(arr.items, arr.len) {
        free_cstring(item);
    }
    arr.items = std::ptr::null_mut();
    arr.len = 0;
}

/// Free a `mycelium_node_info_t` populated by `mycelium_get_node_info`.
/// Releases the inner `subnet` and `pubkey` strings and resets them to
/// NULL.
///
/// # Safety
///
/// `info` must be NULL or point to a struct populated by a successful call
/// to `mycelium_get_node_info`. Calling this twice on the same struct is
/// safe — the second call sees the fields already NULL.
#[no_mangle]
pub unsafe extern "C" fn mycelium_node_info_free(info: *mut mycelium_node_info_t) {
    if info.is_null() {
        return;
    }
    let info = &mut *info;
    free_cstring(info.subnet);
    free_cstring(info.pubkey);
    info.subnet = std::ptr::null_mut();
    info.pubkey = std::ptr::null_mut();
}

/// Free a peer-info array populated by `mycelium_get_peers`. Releases each
/// item's strings, then the backing array, then resets the struct.
///
/// # Safety
///
/// `arr` must be NULL or point to a `mycelium_peer_info_array_t` populated
/// by a successful call to `mycelium_get_peers`. Idempotent.
#[no_mangle]
pub unsafe extern "C" fn mycelium_peer_info_array_free(arr: *mut mycelium_peer_info_array_t) {
    if arr.is_null() {
        return;
    }
    let arr = &mut *arr;
    for p in drain_array(arr.items, arr.len) {
        free_cstring(p.protocol);
        free_cstring(p.address);
        free_cstring(p.peer_type);
        free_cstring(p.connection_state);
    }
    arr.items = std::ptr::null_mut();
    arr.len = 0;
}

/// Free a route array populated by `mycelium_get_selected_routes` or
/// `mycelium_get_fallback_routes`.
///
/// # Safety
///
/// `arr` must be NULL or point to a `mycelium_route_array_t` populated by
/// a successful call to one of those two functions. Idempotent.
#[no_mangle]
pub unsafe extern "C" fn mycelium_route_array_free(arr: *mut mycelium_route_array_t) {
    if arr.is_null() {
        return;
    }
    let arr = &mut *arr;
    for r in drain_array(arr.items, arr.len) {
        free_cstring(r.subnet);
        free_cstring(r.next_hop);
    }
    arr.items = std::ptr::null_mut();
    arr.len = 0;
}

/// Free a queried-subnet array populated by `mycelium_get_queried_subnets`.
///
/// # Safety
///
/// `arr` must be NULL or point to a `mycelium_queried_subnet_array_t`
/// populated by a successful call to `mycelium_get_queried_subnets`.
/// Idempotent.
#[no_mangle]
pub unsafe extern "C" fn mycelium_queried_subnet_array_free(
    arr: *mut mycelium_queried_subnet_array_t,
) {
    if arr.is_null() {
        return;
    }
    let arr = &mut *arr;
    for q in drain_array(arr.items, arr.len) {
        free_cstring(q.subnet);
    }
    arr.items = std::ptr::null_mut();
    arr.len = 0;
}

unsafe fn free_packet_stat_entries(ptr: *mut mycelium_packet_stat_entry_t, len: usize) {
    for e in drain_array(ptr, len) {
        free_cstring(e.ip);
    }
}

/// Free a `mycelium_packet_stats_t` populated by
/// `mycelium_get_packet_stats`. Releases both the by-source and
/// by-destination arrays.
///
/// # Safety
///
/// `stats` must be NULL or point to a `mycelium_packet_stats_t` populated
/// by a successful call to `mycelium_get_packet_stats`. Idempotent.
#[no_mangle]
pub unsafe extern "C" fn mycelium_packet_stats_free(stats: *mut mycelium_packet_stats_t) {
    if stats.is_null() {
        return;
    }
    let stats = &mut *stats;
    free_packet_stat_entries(stats.by_source, stats.by_source_len);
    free_packet_stat_entries(stats.by_destination, stats.by_destination_len);
    stats.by_source = std::ptr::null_mut();
    stats.by_source_len = 0;
    stats.by_destination = std::ptr::null_mut();
    stats.by_destination_len = 0;
}
