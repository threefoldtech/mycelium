//! These are all the exported types and methods in the C FFI interface.
//!
//! Each entry point follows the same shape:
//! 1. Clear the per-thread last-error.
//! 2. Validate inputs; on failure, set the error message and return a
//!    negative status code (or NULL for `mycelium_start`).
//! 3. Lock the handle's internal state; reject if the node is stopped.
//! 4. Use `h.rt().block_on(...)` to bridge the synchronous C caller to the
//!    underlying async APIs.
//! 5. Convert the result into the `repr(C)` mirror type and write through
//!    the out-parameter.

#![allow(non_camel_case_types)]

use core::ffi::c_char;
use std::ffi::CStr;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

use tracing::{error, info, warn};

use super::error::{
    self, MYCELIUM_ERR_INTERNAL, MYCELIUM_ERR_INVALID_ARG, MYCELIUM_ERR_INVALID_STATE, MYCELIUM_OK,
};
use super::types::{
    cstring, mycelium_node_info_t, mycelium_node_t, mycelium_packet_stat_entry_t,
    mycelium_packet_stats_t, mycelium_peer_info_array_t, mycelium_peer_info_t,
    mycelium_queried_subnet_array_t, mycelium_queried_subnet_t, mycelium_route_array_t,
    mycelium_route_t, mycelium_secret_key_t, mycelium_start_config_t, mycelium_string_array_t,
    vec_into_c, NodeState,
};

use crate::crypto;
use crate::metrics::NoMetrics;
use crate::node_handle::{parse_proxy_remote, NodeHandle, DEFAULT_SOCKS_PORT};
use crate::peer_manager::PeerDiscoveryMode;
use crate::Config;

// ---------------------------------------------------------------------------
// Argument-parsing helpers
// ---------------------------------------------------------------------------

unsafe fn cstr_to_str<'a>(ptr: *const c_char, field: &str) -> Result<&'a str, i32> {
    if ptr.is_null() {
        return Err(error::set_and_return(
            format!("{field} is null"),
            MYCELIUM_ERR_INVALID_ARG,
        ));
    }
    CStr::from_ptr(ptr).to_str().map_err(|_| {
        error::set_and_return(
            format!("{field} is not valid UTF-8"),
            MYCELIUM_ERR_INVALID_ARG,
        )
    })
}

unsafe fn cstr_array_to_vec(
    ptr: *const *const c_char,
    len: usize,
    field: &str,
) -> Result<Vec<String>, i32> {
    if len == 0 {
        return Ok(Vec::new());
    }
    if ptr.is_null() {
        return Err(error::set_and_return(
            format!("{field} pointer is null but len > 0"),
            MYCELIUM_ERR_INVALID_ARG,
        ));
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let item = *ptr.add(i);
        let s = cstr_to_str(item, &format!("{field}[{i}]"))?;
        out.push(s.to_owned());
    }
    Ok(out)
}

fn parse_discovery_mode(mode: &str, interfaces: Vec<String>) -> Result<PeerDiscoveryMode, i32> {
    match mode {
        "" | "all" => Ok(PeerDiscoveryMode::All),
        "disabled" => Ok(PeerDiscoveryMode::Disabled),
        "filtered" => Ok(PeerDiscoveryMode::Filtered(interfaces)),
        other => {
            error!("unknown peer_discovery_mode: {other}");
            Err(error::set_and_return(
                format!("unknown peer_discovery_mode: {other}"),
                MYCELIUM_ERR_INVALID_ARG,
            ))
        }
    }
}

/// Borrow the inner running [`NodeHandle`] from a node pointer, or return
/// the appropriate error code if the pointer is NULL or the node has been
/// stopped.
macro_rules! with_running_node {
    ($node:expr, $body:expr) => {{
        if $node.is_null() {
            return error::set_and_return("node is null", MYCELIUM_ERR_INVALID_ARG);
        }
        let node_ref = &*$node;
        let guard = match node_ref.state.lock() {
            Ok(g) => g,
            Err(_) => return error::set_and_return("node lock poisoned", MYCELIUM_ERR_INTERNAL),
        };
        let h: &NodeHandle = match &*guard {
            NodeState::Running(h) => h,
            NodeState::Stopped => {
                return error::set_and_return("node is not running", MYCELIUM_ERR_INVALID_STATE)
            }
        };
        $body(h)
    }};
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Start a mycelium node. Returns an opaque handle on success, or NULL on
/// failure — call `mycelium_last_error_message` for details. The returned
/// handle must eventually be released with `mycelium_node_free`.
///
/// # Safety
///
/// `cfg` must point to a fully-initialised `mycelium_start_config_t`.
/// Pointer fields inside (`peers`, `peer_discovery_mode`,
/// `peer_discovery_interfaces`, `tun_name`) follow the conventions in the
/// module-level safety docs. The struct and any strings it points at are
/// not retained beyond the call — the caller may free them as soon as
/// this function returns.
#[no_mangle]
pub unsafe extern "C" fn mycelium_start(
    cfg: *const mycelium_start_config_t,
) -> *mut mycelium_node_t {
    error::clear();

    let cfg = match cfg.as_ref() {
        Some(c) => c,
        None => {
            error::set("cfg is null");
            return std::ptr::null_mut();
        }
    };

    let peers_strs = match cstr_array_to_vec(cfg.peers, cfg.peers_len, "peers") {
        Ok(v) => v,
        Err(_) => return std::ptr::null_mut(),
    };
    let endpoints = peers_strs.iter().filter_map(|p| p.parse().ok()).collect();

    let mode = match cstr_to_str(cfg.peer_discovery_mode, "peer_discovery_mode") {
        Ok(s) => s.to_owned(),
        Err(_) => return std::ptr::null_mut(),
    };
    let interfaces = match cstr_array_to_vec(
        cfg.peer_discovery_interfaces,
        cfg.peer_discovery_interfaces_len,
        "peer_discovery_interfaces",
    ) {
        Ok(v) => v,
        Err(_) => return std::ptr::null_mut(),
    };
    let peer_discovery_mode = match parse_discovery_mode(&mode, interfaces) {
        Ok(m) => m,
        Err(_) => return std::ptr::null_mut(),
    };

    #[cfg(not(any(target_os = "ios", all(target_os = "macos", feature = "mactunfd"))))]
    let tun_name = match cstr_to_str(cfg.tun_name, "tun_name") {
        Ok(s) => s.to_owned(),
        Err(_) => return std::ptr::null_mut(),
    };

    info!(peers = peers_strs.len(), "starting mycelium node");

    #[cfg(target_os = "android")]
    let tun_fd = match crate::node_handle::create_tun_fd(&tun_name) {
        Ok(fd) => fd,
        Err(e) => {
            error!("Failed to create TUN fd: {e}");
            error::set(format!("failed to create tun fd: {e}"));
            return std::ptr::null_mut();
        }
    };
    #[cfg(any(target_os = "ios", all(target_os = "macos", feature = "mactunfd")))]
    let tun_fd = cfg.tun_fd;

    let config = Config {
        node_key: crypto::SecretKey::from(cfg.priv_key),
        peers: endpoints,
        no_tun: false,
        #[cfg(any(
            target_os = "linux",
            all(target_os = "macos", not(feature = "mactunfd")),
            target_os = "windows"
        ))]
        tun_name,
        #[cfg(any(
            target_os = "android",
            target_os = "ios",
            all(target_os = "macos", feature = "mactunfd"),
        ))]
        tun_fd: Some(tun_fd),
        tcp_listen_port: cfg.tcp_listen_port,
        quic_listen_port: if cfg.quic_listen_port == 0 {
            None
        } else {
            Some(cfg.quic_listen_port)
        },
        peer_discovery_port: cfg.peer_discovery_port,
        peer_discovery_mode,
        metrics: NoMetrics,
        private_network_config: None,
        firewall_mark: None,
        update_workers: 1,
        cdn_cache: None,
        enable_dns: cfg.enable_dns,
        #[cfg(feature = "message")]
        topic_config: None,
        #[cfg(target_os = "linux")]
        vsock_listen_port: None,
    };

    match NodeHandle::start(config) {
        Ok(h) => Box::into_raw(Box::new(mycelium_node_t {
            state: Mutex::new(NodeState::Running(h)),
        })),
        Err(e) => {
            error!("failed to start node: {e}");
            error::set(format!("failed to start node: {e}"));
            std::ptr::null_mut()
        }
    }
}

/// Halt the node and release its internal resources. The handle remains
/// valid for `mycelium_is_running` queries (which will return false) until
/// `mycelium_node_free` is called. No-op on an already-stopped node.
///
/// # Safety
///
/// `node` must be NULL or a live handle returned by `mycelium_start`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_stop(node: *mut mycelium_node_t) -> i32 {
    error::clear();
    if node.is_null() {
        return error::set_and_return("node is null", MYCELIUM_ERR_INVALID_ARG);
    }
    let node = &*node;
    let mut guard = match node.state.lock() {
        Ok(g) => g,
        Err(_) => return error::set_and_return("node lock poisoned", MYCELIUM_ERR_INTERNAL),
    };
    match &mut *guard {
        NodeState::Running(h) => {
            info!("stopping mycelium node");
            h.stop();
            *guard = NodeState::Stopped;
        }
        NodeState::Stopped => {
            warn!("mycelium_stop called on a stopped node");
        }
    }
    MYCELIUM_OK
}

/// Write `true`/`false` into `out` reflecting whether the node is running.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `out` must
/// point to a writable, properly aligned `bool`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_is_running(node: *mut mycelium_node_t, out: *mut bool) -> i32 {
    error::clear();
    if node.is_null() {
        return error::set_and_return("node is null", MYCELIUM_ERR_INVALID_ARG);
    }
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    let node = &*node;
    let guard = match node.state.lock() {
        Ok(g) => g,
        Err(_) => return error::set_and_return("node lock poisoned", MYCELIUM_ERR_INTERNAL),
    };
    *out = matches!(&*guard, NodeState::Running(h) if h.is_running());
    MYCELIUM_OK
}

// ---------------------------------------------------------------------------
// Identity / introspection
// ---------------------------------------------------------------------------

/// Populate `out` with the node's subnet and public key. The returned
/// strings are owned by the caller and must be released with
/// `mycelium_node_info_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `out` must
/// point to a writable, properly aligned `mycelium_node_info_t`. Any
/// previous contents of `*out` are overwritten without being freed —
/// callers must release prior contents first or pass a fresh struct.
#[no_mangle]
pub unsafe extern "C" fn mycelium_get_node_info(
    node: *mut mycelium_node_t,
    out: *mut mycelium_node_info_t,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let info = h.node().lock().await.info();
            *out = mycelium_node_info_t {
                subnet: cstring(info.node_subnet.to_string()),
                pubkey: cstring(info.node_pubkey.to_string()),
            };
            MYCELIUM_OK
        })
    })
}

/// Look up the public key of the peer that owns the given mycelium IP.
/// On success writes a newly-allocated string to `*out` (empty if no
/// match); release with `mycelium_string_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `ip` must
/// be a NUL-terminated C string. `out` must point to a writable
/// `*mut c_char`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_get_public_key_from_ip(
    node: *mut mycelium_node_t,
    ip: *const c_char,
    out: *mut *mut c_char,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    let ip = match cstr_to_str(ip, "ip") {
        Ok(s) => s,
        Err(code) => return code,
    };
    let ip: IpAddr = match ip.parse() {
        Ok(v) => v,
        Err(_) => {
            return error::set_and_return("ip is not a valid address", MYCELIUM_ERR_INVALID_ARG)
        }
    };
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let pubkey = h
                .node()
                .lock()
                .await
                .get_pubkey_from_ip(ip)
                .map(|pk| pk.to_string())
                .unwrap_or_default();
            *out = cstring(pubkey);
            MYCELIUM_OK
        })
    })
}

// ---------------------------------------------------------------------------
// Peers
// ---------------------------------------------------------------------------

/// Populate `out` with one entry per known peer. Release the array with
/// `mycelium_peer_info_array_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `out` must
/// point to a writable, properly aligned `mycelium_peer_info_array_t`;
/// any previous contents are overwritten without being freed.
#[no_mangle]
pub unsafe extern "C" fn mycelium_get_peers(
    node: *mut mycelium_node_t,
    out: *mut mycelium_peer_info_array_t,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let peers: Vec<mycelium_peer_info_t> = h
                .node()
                .lock()
                .await
                .peer_info()
                .into_iter()
                .map(mycelium_peer_info_t::from)
                .collect();
            let (items, len) = vec_into_c(peers);
            *out = mycelium_peer_info_array_t { items, len };
            MYCELIUM_OK
        })
    })
}

/// Add a peer endpoint. Writes `true` to `*out` if the peer was newly
/// added, `false` if it was already known.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `endpoint`
/// must be a NUL-terminated C string. `out` must point to a writable
/// `bool`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_add_peer(
    node: *mut mycelium_node_t,
    endpoint: *const c_char,
    out: *mut bool,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    let endpoint = match cstr_to_str(endpoint, "endpoint") {
        Ok(s) => s,
        Err(code) => return code,
    };
    let endpoint = match endpoint.parse::<crate::endpoint::Endpoint>() {
        Ok(e) => e,
        Err(_) => return error::set_and_return("invalid endpoint", MYCELIUM_ERR_INVALID_ARG),
    };
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            *out = h.node().lock().await.add_peer(endpoint).is_ok();
            MYCELIUM_OK
        })
    })
}

/// Remove a peer endpoint. Writes `true` to `*out` if the peer existed
/// and was removed, `false` if no such peer was registered.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `endpoint`
/// must be a NUL-terminated C string. `out` must point to a writable
/// `bool`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_remove_peer(
    node: *mut mycelium_node_t,
    endpoint: *const c_char,
    out: *mut bool,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    let endpoint = match cstr_to_str(endpoint, "endpoint") {
        Ok(s) => s,
        Err(code) => return code,
    };
    let endpoint = match endpoint.parse::<crate::endpoint::Endpoint>() {
        Ok(e) => e,
        Err(_) => return error::set_and_return("invalid endpoint", MYCELIUM_ERR_INVALID_ARG),
    };
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            *out = h.node().lock().await.remove_peer(endpoint).is_ok();
            MYCELIUM_OK
        })
    })
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Populate `out` with the currently selected (in-use) routes. Release
/// the array with `mycelium_route_array_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `out` must
/// point to a writable, properly aligned `mycelium_route_array_t`; any
/// previous contents are overwritten without being freed.
#[no_mangle]
pub unsafe extern "C" fn mycelium_get_selected_routes(
    node: *mut mycelium_node_t,
    out: *mut mycelium_route_array_t,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let routes: Vec<mycelium_route_t> = h
                .node()
                .lock()
                .await
                .selected_routes()
                .into_iter()
                .map(mycelium_route_t::from)
                .collect();
            let (items, len) = vec_into_c(routes);
            *out = mycelium_route_array_t { items, len };
            MYCELIUM_OK
        })
    })
}

/// Populate `out` with the routing table's fallback (non-selected) routes.
/// Release the array with `mycelium_route_array_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `out` must
/// point to a writable, properly aligned `mycelium_route_array_t`; any
/// previous contents are overwritten without being freed.
#[no_mangle]
pub unsafe extern "C" fn mycelium_get_fallback_routes(
    node: *mut mycelium_node_t,
    out: *mut mycelium_route_array_t,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let routes: Vec<mycelium_route_t> = h
                .node()
                .lock()
                .await
                .fallback_routes()
                .into_iter()
                .map(mycelium_route_t::from)
                .collect();
            let (items, len) = vec_into_c(routes);
            *out = mycelium_route_array_t { items, len };
            MYCELIUM_OK
        })
    })
}

/// Populate `out` with subnets the router is currently querying. Release
/// the array with `mycelium_queried_subnet_array_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `out` must
/// point to a writable, properly aligned `mycelium_queried_subnet_array_t`;
/// any previous contents are overwritten without being freed.
#[no_mangle]
pub unsafe extern "C" fn mycelium_get_queried_subnets(
    node: *mut mycelium_node_t,
    out: *mut mycelium_queried_subnet_array_t,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let now = tokio::time::Instant::now();
            let qs: Vec<mycelium_queried_subnet_t> = h
                .node()
                .lock()
                .await
                .queried_subnets()
                .into_iter()
                .map(|q| mycelium_queried_subnet_t {
                    subnet: cstring(q.subnet().to_string()),
                    expiration_seconds: q.query_expires().saturating_duration_since(now).as_secs()
                        as i64,
                })
                .collect();
            let (items, len) = vec_into_c(qs);
            *out = mycelium_queried_subnet_array_t { items, len };
            MYCELIUM_OK
        })
    })
}

// ---------------------------------------------------------------------------
// Packet statistics
// ---------------------------------------------------------------------------

/// Populate `out` with aggregated packet statistics. Release with
/// `mycelium_packet_stats_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `out` must
/// point to a writable, properly aligned `mycelium_packet_stats_t`; any
/// previous contents are overwritten without being freed.
#[no_mangle]
pub unsafe extern "C" fn mycelium_get_packet_stats(
    node: *mut mycelium_node_t,
    out: *mut mycelium_packet_stats_t,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let stats = h.node().lock().await.packet_statistics();
            let by_source: Vec<mycelium_packet_stat_entry_t> = stats
                .by_source
                .into_iter()
                .map(mycelium_packet_stat_entry_t::from)
                .collect();
            let by_destination: Vec<mycelium_packet_stat_entry_t> = stats
                .by_destination
                .into_iter()
                .map(mycelium_packet_stat_entry_t::from)
                .collect();
            let (src_items, src_len) = vec_into_c(by_source);
            let (dst_items, dst_len) = vec_into_c(by_destination);
            *out = mycelium_packet_stats_t {
                by_source: src_items,
                by_source_len: src_len,
                by_destination: dst_items,
                by_destination_len: dst_len,
            };
            MYCELIUM_OK
        })
    })
}

// ---------------------------------------------------------------------------
// Key utilities (no node required)
// ---------------------------------------------------------------------------

/// Generate a fresh 32-byte node secret key into `out`. Stateless — no
/// node handle required.
///
/// # Safety
///
/// `out` must point to a writable, properly aligned
/// `mycelium_secret_key_t`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_generate_secret_key(out: *mut mycelium_secret_key_t) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    let key = crypto::SecretKey::new();
    *out = mycelium_secret_key_t {
        bytes: *key.as_bytes(),
    };
    MYCELIUM_OK
}

/// Derive the mycelium address (text form) of the public key for the
/// given secret key. Writes a newly-allocated string to `*out`; release
/// with `mycelium_string_free`. Stateless — no node handle required.
///
/// # Safety
///
/// `key` must point to an initialised `mycelium_secret_key_t`. `out` must
/// point to a writable `*mut c_char`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_address_from_secret_key(
    key: *const mycelium_secret_key_t,
    out: *mut *mut c_char,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    let key = match key.as_ref() {
        Some(k) => k,
        None => return error::set_and_return("key is null", MYCELIUM_ERR_INVALID_ARG),
    };
    let secret = crypto::SecretKey::from(key.bytes);
    let address = crypto::PublicKey::from(&secret).address().to_string();
    *out = cstring(address);
    MYCELIUM_OK
}

// ---------------------------------------------------------------------------
// Proxy
// ---------------------------------------------------------------------------

/// Begin the periodic SOCKS5 proxy probe scan.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_start_proxy_probe(node: *mut mycelium_node_t) -> i32 {
    error::clear();
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            h.node().lock().await.start_proxy_scan();
            MYCELIUM_OK
        })
    })
}

/// Stop the periodic SOCKS5 proxy probe scan.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_stop_proxy_probe(node: *mut mycelium_node_t) -> i32 {
    error::clear();
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            h.node().lock().await.stop_proxy_scan();
            MYCELIUM_OK
        })
    })
}

/// List the currently known SOCKS5 proxy endpoints (formatted as
/// `addr:port` strings). Release with `mycelium_string_array_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `out` must
/// point to a writable, properly aligned `mycelium_string_array_t`; any
/// previous contents are overwritten without being freed.
#[no_mangle]
pub unsafe extern "C" fn mycelium_list_proxies(
    node: *mut mycelium_node_t,
    out: *mut mycelium_string_array_t,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let proxies: Vec<*mut c_char> = h
                .node()
                .lock()
                .await
                .known_proxies()
                .into_iter()
                .map(|ip| {
                    let s = SocketAddr::new(IpAddr::from(ip), DEFAULT_SOCKS_PORT).to_string();
                    cstring(s)
                })
                .collect();
            let (items, len) = vec_into_c(proxies);
            *out = mycelium_string_array_t { items, len };
            MYCELIUM_OK
        })
    })
}

/// Connect to a remote SOCKS5 proxy. Writes the negotiated remote address
/// to `*out` as a newly-allocated string; release with
/// `mycelium_string_free`.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`. `remote`
/// must be a NUL-terminated C string. `out` must point to a writable
/// `*mut c_char`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_proxy_connect(
    node: *mut mycelium_node_t,
    remote: *const c_char,
    out: *mut *mut c_char,
) -> i32 {
    error::clear();
    if out.is_null() {
        return error::set_and_return("out is null", MYCELIUM_ERR_INVALID_ARG);
    }
    let remote = match cstr_to_str(remote, "remote") {
        Ok(s) => s,
        Err(code) => return code,
    };
    let remote_addr = match parse_proxy_remote(remote) {
        Ok(r) => r,
        Err(_) => return error::set_and_return("invalid proxy remote", MYCELIUM_ERR_INVALID_ARG),
    };
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            let future = h.node().lock().await.connect_proxy(remote_addr);
            match future.await {
                Ok(addr) => {
                    *out = cstring(addr.to_string());
                    MYCELIUM_OK
                }
                Err(e) => {
                    error!("proxy connect failed: {e}");
                    error::set_and_return(
                        format!("proxy connect failed: {e}"),
                        MYCELIUM_ERR_INTERNAL,
                    )
                }
            }
        })
    })
}

/// Disconnect from the currently connected SOCKS5 proxy, if any.
///
/// # Safety
///
/// `node` must be a live handle returned by `mycelium_start`.
#[no_mangle]
pub unsafe extern "C" fn mycelium_proxy_disconnect(node: *mut mycelium_node_t) -> i32 {
    error::clear();
    with_running_node!(node, |h: &NodeHandle| {
        h.rt().block_on(async {
            h.node().lock().await.disconnect_proxy();
            MYCELIUM_OK
        })
    })
}
