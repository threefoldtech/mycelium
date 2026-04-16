#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use futures::TryStreamExt;
use netlink_packet_route::address::{AddressAttribute, AddressMessage, AddressScope};
use netlink_packet_route::link::{InfoKind, LinkAttribute, LinkInfo, LinkMessage, State};
use netlink_packet_route::AddressFamily;
use rtnetlink::{Handle, LinkBridge, LinkUnspec};
use tokio::task::JoinHandle;

use super::errors::NetworkError;
use super::managed::ManagedState;
use super::models::{
    AddressInfo, BridgeInfo, ListenerInfo, ListenerPolicy, NetworkStatus,
};

type Shared = Arc<Mutex<ManagedState>>;

/// Wraps a rtnetlink handle together with the JoinHandle of its connection
/// task so we can abort the connection task when we are done.
struct NetlinkCtx {
    handle: Handle,
    task: JoinHandle<()>,
}

impl NetlinkCtx {
    async fn new() -> Result<Self, NetworkError> {
        let (conn, handle, _) = rtnetlink::new_connection()
            .map_err(|e| NetworkError::Internal(format!("netlink connection: {e}")))?;
        let task = tokio::spawn(async move {
            conn.await;
        });
        Ok(Self { handle, task })
    }
}

impl Drop for NetlinkCtx {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn map_rtnl_err(e: rtnetlink::Error) -> NetworkError {
    if let rtnetlink::Error::NetlinkError(ref em) = e {
        let code = em.raw_code().abs();
        match code {
            libc::EPERM | libc::EACCES => return NetworkError::PermissionDenied,
            libc::EEXIST => return NetworkError::AddressAlreadyExists,
            libc::ENODEV => return NetworkError::InterfaceNotFound,
            libc::ENOENT => return NetworkError::InterfaceNotFound,
            _ => {}
        }
    }
    NetworkError::Internal(e.to_string())
}

pub fn is_mycelium_range(a: Ipv6Addr) -> bool {
    (a.segments()[0] & 0xFE00) == 0x0400
}

fn link_is_bridge(msg: &LinkMessage) -> bool {
    for attr in &msg.attributes {
        if let LinkAttribute::LinkInfo(infos) = attr {
            for info in infos {
                if let LinkInfo::Kind(k) = info {
                    if matches!(k, InfoKind::Bridge) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn link_kind(msg: &LinkMessage) -> Option<InfoKind> {
    for attr in &msg.attributes {
        if let LinkAttribute::LinkInfo(infos) = attr {
            for info in infos {
                if let LinkInfo::Kind(k) = info {
                    return Some(k.clone());
                }
            }
        }
    }
    None
}

fn link_name(msg: &LinkMessage) -> Option<String> {
    for attr in &msg.attributes {
        if let LinkAttribute::IfName(n) = attr {
            return Some(n.clone());
        }
    }
    None
}

fn link_controller(msg: &LinkMessage) -> Option<u32> {
    for attr in &msg.attributes {
        if let LinkAttribute::Controller(idx) = attr {
            return Some(*idx);
        }
    }
    None
}

fn link_state_string(msg: &LinkMessage) -> String {
    for attr in &msg.attributes {
        if let LinkAttribute::OperState(s) = attr {
            return match s {
                State::Unknown => "unknown".into(),
                State::NotPresent => "not_present".into(),
                State::Down => "down".into(),
                State::LowerLayerDown => "lower_layer_down".into(),
                State::Testing => "testing".into(),
                State::Dormant => "dormant".into(),
                State::Up => "up".into(),
                State::Other(v) => format!("other({v})"),
                _ => "unknown".into(),
            };
        }
    }
    "unknown".into()
}

fn scope_string(scope: AddressScope) -> String {
    match scope {
        AddressScope::Universe => "global".into(),
        AddressScope::Site => "site".into(),
        AddressScope::Link => "link".into(),
        AddressScope::Host => "host".into(),
        AddressScope::Nowhere => "nowhere".into(),
        AddressScope::Other(v) => format!("other({v})"),
        _ => "unknown".into(),
    }
}

fn family_string(f: AddressFamily) -> String {
    match f {
        AddressFamily::Inet => "ipv4".into(),
        AddressFamily::Inet6 => "ipv6".into(),
        other => format!("{:?}", other).to_lowercase(),
    }
}

fn addr_from_message(msg: &AddressMessage) -> Option<IpAddr> {
    for nla in &msg.attributes {
        if let AddressAttribute::Address(ip) = nla {
            return Some(*ip);
        }
    }
    for nla in &msg.attributes {
        if let AddressAttribute::Local(ip) = nla {
            return Some(*ip);
        }
    }
    None
}

async fn list_links(handle: &Handle) -> Result<Vec<LinkMessage>, NetworkError> {
    let mut stream = handle.link().get().execute();
    let mut out = Vec::new();
    while let Some(msg) = stream.try_next().await.map_err(map_rtnl_err)? {
        out.push(msg);
    }
    Ok(out)
}

async fn list_all_addresses(handle: &Handle) -> Result<Vec<AddressMessage>, NetworkError> {
    let mut stream = handle.address().get().execute();
    let mut out = Vec::new();
    while let Some(msg) = stream.try_next().await.map_err(map_rtnl_err)? {
        out.push(msg);
    }
    Ok(out)
}

async fn get_link_by_name(
    handle: &Handle,
    name: &str,
) -> Result<Option<LinkMessage>, NetworkError> {
    let mut stream = handle
        .link()
        .get()
        .match_name(name.to_string())
        .execute();
    match stream.try_next().await {
        Ok(v) => Ok(v),
        Err(rtnetlink::Error::NetlinkError(em)) if em.raw_code().abs() == libc::ENODEV => Ok(None),
        Err(e) => Err(map_rtnl_err(e)),
    }
}

fn managed_snapshot(
    managed: &Shared,
) -> Result<
    (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<(String, IpAddr, u8)>,
        ListenerPolicy,
        Vec<String>,
    ),
    NetworkError,
> {
    let g = managed
        .lock()
        .map_err(|e| NetworkError::Internal(e.to_string()))?;
    Ok((
        g.bridges.clone(),
        g.addresses.clone(),
        g.policy,
        g.explicit_addresses.clone(),
    ))
}

fn build_address_info(
    msg: &AddressMessage,
    iface_name: &str,
    managed_set: &std::collections::BTreeSet<(String, IpAddr, u8)>,
    active_listener: bool,
) -> Option<AddressInfo> {
    let ip = addr_from_message(msg)?;
    let prefix = msg.header.prefix_len;
    let managed_flag = managed_set.contains(&(iface_name.to_string(), ip, prefix));
    Some(AddressInfo {
        interface: iface_name.to_string(),
        ifindex: msg.header.index,
        address: ip.to_string(),
        prefix_len: prefix,
        scope: scope_string(msg.header.scope),
        family: family_string(msg.header.family),
        managed: managed_flag,
        active_listener,
    })
}

pub async fn list_bridges(
    managed: Shared,
    include_addresses: bool,
    include_ports: bool,
    managed_only: bool,
) -> Result<Vec<BridgeInfo>, NetworkError> {
    let ctx = NetlinkCtx::new().await?;
    let (managed_bridges, managed_addrs, _policy, _explicit) = managed_snapshot(&managed)?;

    let links = list_links(&ctx.handle).await?;

    let name_by_idx: HashMap<u32, String> = links
        .iter()
        .filter_map(|l| link_name(l).map(|n| (l.header.index, n)))
        .collect();

    let addrs = if include_addresses {
        list_all_addresses(&ctx.handle).await?
    } else {
        Vec::new()
    };

    let mut out = Vec::new();
    for link in &links {
        if !link_is_bridge(link) {
            continue;
        }
        let name = match link_name(link) {
            Some(n) => n,
            None => continue,
        };
        let managed_flag = managed_bridges.contains(&name);
        if managed_only && !managed_flag {
            continue;
        }

        let ports = if include_ports {
            links
                .iter()
                .filter(|l| link_controller(l) == Some(link.header.index))
                .filter_map(link_name)
                .collect()
        } else {
            Vec::new()
        };

        let addresses = if include_addresses {
            addrs
                .iter()
                .filter(|a| a.header.index == link.header.index)
                .filter_map(|a| {
                    let iface = name_by_idx
                        .get(&a.header.index)
                        .cloned()
                        .unwrap_or_default();
                    build_address_info(a, &iface, &managed_addrs, false)
                })
                .collect()
        } else {
            Vec::new()
        };

        out.push(BridgeInfo {
            name,
            ifindex: link.header.index,
            state: link_state_string(link),
            managed: managed_flag,
            ports,
            addresses,
        });
    }

    Ok(out)
}

pub async fn ensure_bridge(
    managed: Shared,
    name: String,
    up: bool,
    mtu: Option<u32>,
) -> Result<(BridgeInfo, bool), NetworkError> {
    if name.is_empty() || name.len() > 15 || name.contains('/') || name.contains(' ') {
        return Err(NetworkError::InvalidBridgeName);
    }
    let ctx = NetlinkCtx::new().await?;

    let existing = get_link_by_name(&ctx.handle, &name).await?;

    let (info, created) = if let Some(existing) = existing {
        if !link_is_bridge(&existing) {
            return Err(NetworkError::InvalidInterfaceType);
        }
        let idx = existing.header.index;

        if let Some(mtu) = mtu {
            let msg = LinkUnspec::new_with_index(idx).mtu(mtu).build();
            ctx.handle
                .link()
                .set(msg)
                .execute()
                .await
                .map_err(map_rtnl_err)?;
        }
        if up {
            let msg = LinkUnspec::new_with_index(idx).up().build();
            ctx.handle
                .link()
                .set(msg)
                .execute()
                .await
                .map_err(map_rtnl_err)?;
        }

        let refreshed = get_link_by_name(&ctx.handle, &name).await?.unwrap_or(existing);
        let info = bridge_info_from_link(&refreshed, &managed, &ctx.handle).await?;
        (info, false)
    } else {
        let mut builder = LinkBridge::new(&name);
        if let Some(mtu) = mtu {
            builder = builder.mtu(mtu);
        }
        // LinkBridge::new already sets Up; if caller wanted down, we'd have to override.
        // The public contract defaults to up=true anyway.
        let _ = up;

        ctx.handle
            .link()
            .add(builder.build())
            .execute()
            .await
            .map_err(map_rtnl_err)?;

        let fresh = get_link_by_name(&ctx.handle, &name)
            .await?
            .ok_or(NetworkError::Internal("bridge not found after create".into()))?;

        {
            let mut g = managed
                .lock()
                .map_err(|e| NetworkError::Internal(e.to_string()))?;
            g.bridges.insert(name.clone());
        }

        let info = bridge_info_from_link(&fresh, &managed, &ctx.handle).await?;
        (info, true)
    };

    Ok((info, created))
}

async fn bridge_info_from_link(
    link: &LinkMessage,
    managed: &Shared,
    handle: &Handle,
) -> Result<BridgeInfo, NetworkError> {
    let (managed_bridges, managed_addrs, _p, _e) = managed_snapshot(managed)?;
    let name = link_name(link).unwrap_or_default();
    let links = list_links(handle).await?;
    let ports: Vec<String> = links
        .iter()
        .filter(|l| link_controller(l) == Some(link.header.index))
        .filter_map(link_name)
        .collect();
    let addrs: Vec<AddressInfo> = list_all_addresses(handle)
        .await?
        .iter()
        .filter(|a| a.header.index == link.header.index)
        .filter_map(|a| build_address_info(a, &name, &managed_addrs, false))
        .collect();
    Ok(BridgeInfo {
        name: name.clone(),
        ifindex: link.header.index,
        state: link_state_string(link),
        managed: managed_bridges.contains(&name),
        ports,
        addresses: addrs,
    })
}

pub async fn delete_bridge(
    managed: Shared,
    name: String,
    only_if_empty: bool,
) -> Result<bool, NetworkError> {
    let ctx = NetlinkCtx::new().await?;

    let link = match get_link_by_name(&ctx.handle, &name).await? {
        Some(l) => l,
        None => return Err(NetworkError::BridgeNotFound),
    };
    if !link_is_bridge(&link) {
        return Err(NetworkError::InvalidInterfaceType);
    }
    let idx = link.header.index;

    if only_if_empty {
        let links = list_links(&ctx.handle).await?;
        let has_port = links.iter().any(|l| link_controller(l) == Some(idx));
        let addrs = list_all_addresses(&ctx.handle).await?;
        let has_addr = addrs.iter().any(|a| a.header.index == idx);
        if has_port || has_addr {
            return Err(NetworkError::BridgeNotEmpty);
        }
    }

    ctx.handle
        .link()
        .del(idx)
        .execute()
        .await
        .map_err(map_rtnl_err)?;

    {
        let mut g = managed
            .lock()
            .map_err(|e| NetworkError::Internal(e.to_string()))?;
        g.bridges.remove(&name);
        g.addresses.retain(|(iface, _, _)| iface != &name);
    }

    Ok(true)
}

pub async fn list_addresses(
    managed: Shared,
    interface: Option<String>,
    bridge_only: bool,
    mycelium_only: bool,
    managed_only: bool,
) -> Result<Vec<AddressInfo>, NetworkError> {
    let ctx = NetlinkCtx::new().await?;
    let (_bridges, managed_addrs, _policy, _explicit) = managed_snapshot(&managed)?;

    let links = list_links(&ctx.handle).await?;
    let name_by_idx: HashMap<u32, String> = links
        .iter()
        .filter_map(|l| link_name(l).map(|n| (l.header.index, n)))
        .collect();
    let bridge_idx: std::collections::BTreeSet<u32> = links
        .iter()
        .filter(|l| link_is_bridge(l))
        .map(|l| l.header.index)
        .collect();

    let iface_idx: Option<u32> = match interface {
        Some(n) => Some(
            links
                .iter()
                .find(|l| link_name(l).as_deref() == Some(n.as_str()))
                .ok_or(NetworkError::InterfaceNotFound)?
                .header
                .index,
        ),
        None => None,
    };

    let all = list_all_addresses(&ctx.handle).await?;
    let mut out = Vec::new();
    for a in &all {
        if let Some(idx) = iface_idx {
            if a.header.index != idx {
                continue;
            }
        }
        if bridge_only && !bridge_idx.contains(&a.header.index) {
            continue;
        }
        let name = name_by_idx
            .get(&a.header.index)
            .cloned()
            .unwrap_or_default();
        if mycelium_only {
            match addr_from_message(a) {
                Some(IpAddr::V6(v6)) if is_mycelium_range(v6) => {}
                _ => continue,
            }
        }
        if let Some(info) = build_address_info(a, &name, &managed_addrs, false) {
            if managed_only && !info.managed {
                continue;
            }
            out.push(info);
        }
    }
    Ok(out)
}

pub async fn add_address(
    managed: Shared,
    interface: String,
    address: Ipv6Addr,
    prefix_len: u8,
    activate_listener: bool,
) -> Result<(String, String, u8, bool, bool), NetworkError> {
    if !is_mycelium_range(address) {
        return Err(NetworkError::AddressOutOfRange);
    }
    if prefix_len > 128 {
        return Err(NetworkError::InvalidArgument("prefix_len > 128".into()));
    }

    let ctx = NetlinkCtx::new().await?;
    let link = get_link_by_name(&ctx.handle, &interface)
        .await?
        .ok_or(NetworkError::InterfaceNotFound)?;
    if !matches!(link_kind(&link), Some(InfoKind::Bridge)) {
        return Err(NetworkError::InvalidInterfaceType);
    }

    ctx.handle
        .address()
        .add(link.header.index, IpAddr::V6(address), prefix_len)
        .execute()
        .await
        .map_err(|e| match e {
            rtnetlink::Error::NetlinkError(ref em)
                if em.raw_code().abs() == libc::EEXIST =>
            {
                NetworkError::AddressAlreadyExists
            }
            other => map_rtnl_err(other),
        })?;

    {
        let mut g = managed
            .lock()
            .map_err(|e| NetworkError::Internal(e.to_string()))?;
        g.addresses
            .insert((interface.clone(), IpAddr::V6(address), prefix_len));
    }

    Ok((
        interface,
        address.to_string(),
        prefix_len,
        true,
        activate_listener,
    ))
}

pub async fn remove_address(
    managed: Shared,
    interface: String,
    address: String,
) -> Result<(bool, bool), NetworkError> {
    let (ip_wanted, prefix_hint) = parse_address_with_prefix(&address, None)?;

    let ctx = NetlinkCtx::new().await?;
    let link = get_link_by_name(&ctx.handle, &interface)
        .await?
        .ok_or(NetworkError::InterfaceNotFound)?;

    let addrs = list_all_addresses(&ctx.handle).await?;
    let matched: Option<AddressMessage> = addrs.into_iter().find(|a| {
        if a.header.index != link.header.index {
            return false;
        }
        if let Some(ip) = addr_from_message(a) {
            if ip != IpAddr::V6(ip_wanted) {
                return false;
            }
            if let Some(p) = prefix_hint {
                if a.header.prefix_len != p {
                    return false;
                }
            }
            return true;
        }
        false
    });

    let am = matched.ok_or(NetworkError::AddressNotFound)?;
    let prefix = am.header.prefix_len;
    ctx.handle
        .address()
        .del(am)
        .execute()
        .await
        .map_err(map_rtnl_err)?;

    let listener_removed = {
        let mut g = managed
            .lock()
            .map_err(|e| NetworkError::Internal(e.to_string()))?;
        g.addresses
            .remove(&(interface.clone(), IpAddr::V6(ip_wanted), prefix))
    };

    Ok((true, listener_removed))
}

pub async fn get_listeners(
    managed: Shared,
) -> Result<(String, Vec<ListenerInfo>), NetworkError> {
    let (_bridges, _managed_addrs, policy, explicit) = managed_snapshot(&managed)?;

    // Collect system listeners from /proc/net/{tcp6,udp6}.
    let mut listeners: Vec<ListenerInfo> = Vec::new();
    let tcp6 = tokio::fs::read_to_string("/proc/net/tcp6").await.ok();
    let udp6 = tokio::fs::read_to_string("/proc/net/udp6").await.ok();

    let ctx = NetlinkCtx::new().await?;
    let links = list_links(&ctx.handle).await?;
    let name_by_idx: HashMap<u32, String> = links
        .iter()
        .filter_map(|l| link_name(l).map(|n| (l.header.index, n)))
        .collect();
    let all_addrs = list_all_addresses(&ctx.handle).await?;

    // Map of (IpAddr -> interface name), used to attribute a listener to a nic.
    let mut ip_to_iface: HashMap<IpAddr, String> = HashMap::new();
    for a in &all_addrs {
        if let Some(ip) = addr_from_message(a) {
            if let Some(name) = name_by_idx.get(&a.header.index) {
                ip_to_iface.entry(ip).or_insert_with(|| name.clone());
            }
        }
    }

    if let Some(body) = tcp6 {
        listeners.extend(parse_proc_net_listeners(&body, "tcp", &ip_to_iface, true));
    }
    if let Some(body) = udp6 {
        listeners.extend(parse_proc_net_listeners(&body, "udp", &ip_to_iface, false));
    }

    // Tag each listener "active" according to policy.
    for l in &mut listeners {
        let ip: IpAddr = l.address.parse().unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        l.active = match policy {
            ListenerPolicy::AllMyceliumAddresses => match ip {
                IpAddr::V6(v6) => is_mycelium_range(v6),
                _ => false,
            },
            ListenerPolicy::AllManagedAddresses => {
                // active if this listener IP matches any managed address
                let snap = managed
                    .lock()
                    .map_err(|e| NetworkError::Internal(e.to_string()))?;
                snap.addresses.iter().any(|(_iface, mip, _p)| *mip == ip)
            }
            ListenerPolicy::ExplicitOnly => explicit.contains(&l.address),
        };
    }

    Ok((policy.as_str().to_string(), listeners))
}

pub async fn set_listener_policy(
    managed: Shared,
    policy: ListenerPolicy,
    explicit: Option<Vec<String>>,
) -> Result<String, NetworkError> {
    let mut g = managed
        .lock()
        .map_err(|e| NetworkError::Internal(e.to_string()))?;
    g.policy = policy;
    if let Some(e) = explicit {
        g.explicit_addresses = e;
    }
    Ok(g.policy.as_str().to_string())
}

pub async fn get_status(managed: Shared) -> Result<NetworkStatus, NetworkError> {
    let ctx = NetlinkCtx::new().await?;
    let (managed_bridges, managed_addrs, policy, _explicit) = managed_snapshot(&managed)?;

    let links = list_links(&ctx.handle).await?;
    let bridges = links.iter().filter(|l| link_is_bridge(l)).count() as u32;

    let addrs = list_all_addresses(&ctx.handle).await?;
    let mut mycelium_count: u32 = 0;
    for a in &addrs {
        if let Some(IpAddr::V6(v6)) = addr_from_message(a) {
            if is_mycelium_range(v6) {
                mycelium_count += 1;
            }
        }
    }

    drop(ctx);
    let (_p, listeners) = get_listeners(Arc::clone(&managed)).await?;
    let active_listeners = listeners.iter().filter(|l| l.active).count() as u32;

    let managed_bridge_count = managed_bridges.len() as u32;
    let _ = managed_addrs;

    Ok(NetworkStatus {
        platform: "linux".to_string(),
        listener_policy: policy.as_str().to_string(),
        supports_bridge_management: true,
        supports_address_management: true,
        bridges,
        managed_bridges: managed_bridge_count,
        mycelium_addresses: mycelium_count,
        active_listeners,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn parse_address_with_prefix(
    s: &str,
    default: Option<u8>,
) -> Result<(Ipv6Addr, Option<u8>), NetworkError> {
    if let Some((addr, p)) = s.split_once('/') {
        let ip: Ipv6Addr = addr
            .parse()
            .map_err(|_| NetworkError::InvalidArgument(format!("invalid IPv6 address: {addr}")))?;
        let prefix: u8 = p
            .parse()
            .map_err(|_| NetworkError::InvalidArgument(format!("invalid prefix: {p}")))?;
        if prefix > 128 {
            return Err(NetworkError::InvalidArgument("prefix > 128".into()));
        }
        Ok((ip, Some(prefix)))
    } else {
        let ip: Ipv6Addr = s
            .parse()
            .map_err(|_| NetworkError::InvalidArgument(format!("invalid IPv6 address: {s}")))?;
        Ok((ip, default))
    }
}

/// Parse hex IPv6 from /proc/net/tcp6 (or udp6) of form
/// "HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHH:PPPP". The 32 hex chars encode 4 u32
/// little-endian words.
fn parse_proc_ipv6(s: &str) -> Option<(Ipv6Addr, u16)> {
    let (addr, port) = s.split_once(':')?;
    if addr.len() != 32 {
        return None;
    }
    let port = u16::from_str_radix(port, 16).ok()?;
    let mut bytes = [0u8; 16];
    for w in 0..4 {
        let slice = &addr[w * 8..w * 8 + 8];
        let word = u32::from_str_radix(slice, 16).ok()?;
        let le = word.to_le_bytes();
        bytes[w * 4..w * 4 + 4].copy_from_slice(&le);
    }
    Some((Ipv6Addr::from(bytes), port))
}

fn parse_proc_net_listeners(
    body: &str,
    source: &str,
    ip_to_iface: &HashMap<IpAddr, String>,
    tcp_only_listen: bool,
) -> Vec<ListenerInfo> {
    let mut out = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if i == 0 {
            continue;
        }
        let mut cols = line.split_whitespace();
        let _sl = cols.next();
        let local = match cols.next() {
            Some(c) => c,
            None => continue,
        };
        let _rem = cols.next();
        let state = match cols.next() {
            Some(s) => s,
            None => continue,
        };
        if tcp_only_listen && state != "0A" {
            continue;
        }
        let (ip, port) = match parse_proc_ipv6(local) {
            Some(v) => v,
            None => continue,
        };
        let iface = ip_to_iface
            .get(&IpAddr::V6(ip))
            .cloned()
            .unwrap_or_else(|| {
                if ip.is_unspecified() {
                    "*".to_string()
                } else {
                    String::new()
                }
            });
        out.push(ListenerInfo {
            interface: iface,
            address: ip.to_string(),
            port,
            source: source.to_string(),
            active: false,
        });
    }
    out
}
