use std::{
    net::{IpAddr, Ipv6Addr},
    ops::Deref,
    sync::{Arc, Mutex, MutexGuard},
};

use arc_swap::ArcSwap;
use ip_network_table_deps_treebitmap::IpLookupTable;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, trace};

use crate::{crypto::SharedSecret, peer::Peer, subnet::Subnet};

pub use iter::RoutingTableIter;
pub use iter_mut::RoutingTableIterMut;
pub use route_entry::RouteEntry;
pub use route_key::RouteKey;
pub use route_list::RouteList;

mod iter;
mod iter_mut;
mod route_entry;
mod route_key;
mod route_list;

/// The routing table holds a list of route entries for every known subnet.
#[derive(Clone)]
pub struct RoutingTable {
    writer: Arc<Mutex<left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>>,
    reader: left_right::ReadHandle<RoutingTableInner>,

    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancel_token: CancellationToken,
}

#[derive(Default)]
struct RoutingTableInner {
    table: IpLookupTable<Ipv6Addr, Arc<ArcSwap<RouteList>>>,
}

/// Hold an exclusive write lock over the routing table. While this item is in scope, no other
/// calls can get a mutable refernce to the content of a routing table. Once this guard goes out of
/// scope, changes to the contained RouteList will be applied.
pub struct WriteGuard<'a> {
    routing_table: &'a RoutingTable,
    /// Owned copy of the RouteList, this is populated once mutable access the the RouteList has
    /// been requested.
    value: Arc<ArcSwap<RouteList>>,
    /// Did the RouteList exist initially?
    exists: bool,
    /// The subnet we are writing to.
    subnet: Subnet,
    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancellation_token: CancellationToken,
}

impl RoutingTable {
    /// Create a new empty RoutingTable. The passed channel is used to notify an external observer
    /// of route entry expiration events. It is the callers responsibility to ensure these events
    /// are properly handled.
    ///
    /// # Panics
    ///
    /// This will panic if not executed in the context of a tokio runtime.
    pub fn new(expired_route_entry_sink: mpsc::Sender<RouteKey>) -> Self {
        let (writer, reader) = left_right::new();
        let writer = Arc::new(Mutex::new(writer));

        let cancel_token = CancellationToken::new();

        RoutingTable {
            writer,
            reader,
            expired_route_entry_sink,
            cancel_token,
        }
    }

    /// Get a list of the routes for the most precises [`Subnet`] known which contains the given
    /// [`IpAddr`].
    pub fn best_routes(&self, ip: IpAddr) -> Option<RouteListReadGuard> {
        let IpAddr::V6(ip) = ip else {
            panic!("Only IPv6 is supported currently");
        };
        self.reader
            .enter()
            .expect("Write handle is saved on the router so it is not dropped yet.")
            .table
            .longest_match(ip)
            .map(|(_, _, rl)| RouteListReadGuard { inner: rl.load() })
    }

    /// Get a list of all routes for the given subnet. Changes to the RoutingTable after this
    /// method returns will not be visible and require this method to be called again to be
    /// observed.
    pub fn routes(&self, subnet: Subnet) -> Option<RouteListReadGuard> {
        let subnet_ip = if let IpAddr::V6(ip) = subnet.address() {
            ip
        } else {
            return None;
        };

        self.reader
            .enter()
            .expect("Write handle is saved on the router so it is not dropped yet.")
            .table
            .exact_match(subnet_ip, subnet.prefix_len().into())
            .map(|rl| RouteListReadGuard { inner: rl.load() })
    }

    /// Gets continued read access to the `RoutingTable`. While the returned
    /// [`guard`](RoutingTableReadGuard) is held, updates to the `RoutingTable` will be blocked.
    pub fn read(&self) -> RoutingTableReadGuard {
        RoutingTableReadGuard {
            guard: self
                .reader
                .enter()
                .expect("Write handle is saved on RoutingTable, so this is always Some; qed"),
        }
    }

    /// Locks the `RoutingTable` for continued write access. While the returned
    /// [`guard`](RoutingTableWriteGuard) is held, methods trying to mutate the `RoutingTable`, or
    /// get mutable access otherwise, will be blocked. When the [`guard`](`RoutingTableWriteGuard`)
    /// is dropped, all queued changes will be applied.
    pub fn write(&self) -> RoutingTableWriteGuard {
        RoutingTableWriteGuard {
            write_guard: self.writer.lock().unwrap(),
            read_guard: self
                .reader
                .enter()
                .expect("Write handle is saved on RoutingTable, so this is always Some; qed"),
            expired_route_entry_sink: self.expired_route_entry_sink.clone(),
            cancel_token: self.cancel_token.clone(),
        }
    }

    /// Get mutable access to the list of routes for the given [`Subnet`].
    pub fn routes_mut(&self, subnet: Subnet) -> Option<WriteGuard> {
        let subnet_address = if let IpAddr::V6(ip) = subnet.address() {
            ip
        } else {
            panic!("IP v4 addresses are not supported")
        };

        let value = self
            .reader
            .enter()
            .expect("Write handle is saved next to read handle so this is always Some; qed")
            .table
            .exact_match(subnet_address, subnet.prefix_len().into())?
            .clone();

        Some(WriteGuard {
            routing_table: self,
            // If we didn't find a route list in the route table we create a new empty list,
            // therefore we immediately own it.
            value,
            exists: true,
            subnet,
            expired_route_entry_sink: self.expired_route_entry_sink.clone(),
            cancellation_token: self.cancel_token.clone(),
        })
    }

    /// Adds a new [`Subnet`] to the `RoutingTable`. The returned [`WriteGuard`] can be used to
    /// insert entries. If no entry is inserted before the guard is dropped, the [`Subnet`] won't
    /// be added.
    pub fn add_subnet(&self, subnet: Subnet, shared_secret: SharedSecret) -> WriteGuard {
        if !matches!(subnet.address(), IpAddr::V6(_)) {
            panic!("IP v4 addresses are not supported")
        };

        let value = Arc::new(Arc::new(RouteList::new(shared_secret)).into());

        WriteGuard {
            routing_table: self,
            value,
            exists: false,
            subnet,
            expired_route_entry_sink: self.expired_route_entry_sink.clone(),
            cancellation_token: self.cancel_token.clone(),
        }
    }

    /// Gets the selected route for an IpAddr if one exists.
    ///
    /// # Panics
    ///
    /// This will panic if the IP address is not an IPV6 address.
    pub fn selected_route(&self, address: IpAddr) -> Option<RouteEntry> {
        let IpAddr::V6(ip) = address else {
            panic!("IP v4 addresses are not supported")
        };
        self.reader
            .enter()
            .expect("Write handle is saved on RoutingTable, so this is always Some; qed")
            .table
            .longest_match(ip)
            .and_then(|(_, _, rl)| {
                let rl = rl.load();
                if rl.is_empty() || !rl[0].selected() {
                    None
                } else {
                    Some(rl[0].clone())
                }
            })
    }
}

pub struct RouteListReadGuard {
    inner: arc_swap::Guard<Arc<RouteList>>,
}

impl Deref for RouteListReadGuard {
    type Target = RouteList;

    fn deref(&self) -> &Self::Target {
        self.inner.deref()
    }
}

/// A write guard over the [`RoutingTable`]. While this guard is held, updates won't be able to
/// complete.
pub struct RoutingTableWriteGuard<'a> {
    write_guard: MutexGuard<'a, left_right::WriteHandle<RoutingTableInner, RoutingTableOplogEntry>>,
    read_guard: left_right::ReadGuard<'a, RoutingTableInner>,
    expired_route_entry_sink: mpsc::Sender<RouteKey>,
    cancel_token: CancellationToken,
}

impl<'a, 'b> RoutingTableWriteGuard<'a> {
    pub fn iter_mut(&'b mut self) -> RoutingTableIterMut<'a, 'b> {
        RoutingTableIterMut::new(
            &mut self.write_guard,
            self.read_guard.table.iter(),
            self.expired_route_entry_sink.clone(),
            self.cancel_token.clone(),
        )
    }
}

impl Drop for RoutingTableWriteGuard<'_> {
    fn drop(&mut self) {
        self.write_guard.publish();
    }
}

/// A read guard over the [`RoutingTable`]. While this guard is held, updates won't be able to
/// complete.
pub struct RoutingTableReadGuard<'a> {
    guard: left_right::ReadGuard<'a, RoutingTableInner>,
}

impl RoutingTableReadGuard<'_> {
    pub fn iter(&self) -> RoutingTableIter {
        RoutingTableIter::new(self.guard.table.iter())
    }
}

impl WriteGuard<'_> {
    /// Loads the current [`RouteList`].
    #[inline]
    pub fn routes(&self) -> RouteListReadGuard {
        RouteListReadGuard {
            inner: self.value.load(),
        }
    }

    /// Get mutable access to the [`RouteList`]. This will update the [`RouteList`] in place
    /// without locking the [`RoutingTable`].
    // TODO: Proper abstractions
    pub fn update_routes<
        F: FnMut(&mut RouteList, &mpsc::Sender<RouteKey>, &CancellationToken) -> bool,
    >(
        &mut self,
        mut op: F,
    ) -> bool {
        let mut res = false;
        let mut delete = false;
        self.value.rcu(|rl| {
            let mut new_val = rl.clone();
            let v = Arc::make_mut(&mut new_val);

            res = op(v, &self.expired_route_entry_sink, &self.cancellation_token);
            delete = v.is_empty();

            new_val
        });

        if delete && self.exists {
            trace!(subnet = %self.subnet, "Deleting subnet which became empty after updating");
            let mut writer = self.routing_table.writer.lock().unwrap();

            writer.append(RoutingTableOplogEntry::Delete(self.subnet));
            writer.publish();
        }

        res
    }

    /// Set the [`RouteEntry`] with the given [`neighbour`](Peer) as the selected route.
    pub fn set_selected(&mut self, neighbour: &Peer) {
        self.value.rcu(|routes| {
            let mut new_routes = routes.clone();
            let routes = Arc::make_mut(&mut new_routes);
            let Some(pos) = routes.iter().position(|re| re.neighbour() == neighbour) else {
                error!(
                    neighbour = neighbour.connection_identifier(),
                    "Failed to select route entry with given route key, no such entry"
                );
                return new_routes;
            };

            // We don't need a check for an empty list here, since we found a selected route there
            // _MUST_ be at least 1 entry.
            // Set the first element to unselected, then select the proper element so this also works
            // in case the existing route is "reselected".
            routes[0].set_selected(false);
            routes[pos].set_selected(true);
            routes.swap(0, pos);

            new_routes
        });
    }

    /// Unconditionally unselects the selected route, if one is present.
    ///
    /// In case no route is selected, this is a no-op.
    pub fn unselect(&mut self) {
        self.value.rcu(|v| {
            let mut new_val = v.clone();
            let new_ref = Arc::make_mut(&mut new_val);

            if let Some(e) = new_ref.get_mut(0) {
                e.set_selected(false);
            }

            new_val
        });
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        // FIXME: try to get rid of clones on the Arc here
        let value = self.value.load();
        match self.exists {
            // The route list did not exist, and now it is not empty, so an entry was added. We
            // need to add the route list to the routing table.
            false if !value.is_empty() => {
                trace!(subnet = %self.subnet, "Inserting new route list for subnet");
                let mut writer = self.routing_table.writer.lock().unwrap();
                writer.append(RoutingTableOplogEntry::Upsert(
                    self.subnet,
                    Arc::clone(&self.value),
                ));
                writer.publish();
            }
            // There was an existing route list which is now empty, so the entry for this subnet
            // needs to be deleted in the routing table.
            true if value.is_empty() => {
                trace!(subnet = %self.subnet, "Removing route list for subnet");
                let mut writer = self.routing_table.writer.lock().unwrap();
                writer.append(RoutingTableOplogEntry::Delete(self.subnet));
                writer.publish();
            }
            // Nothing to do in these cases. Either no value was inserted in a non existing
            // routelist, or an existing one was updated in place.
            _ => {}
        }
    }
}

/// Operations allowed on the left_right for the routing table.
enum RoutingTableOplogEntry {
    /// Insert or Update the value for the given subnet.
    Upsert(Subnet, Arc<ArcSwap<RouteList>>),
    /// Delete the entry for the given subnet.
    Delete(Subnet),
}

/// Convert an [`IpAddr`] into an [`Ipv6Addr`]. Panics if the contained addrss is not an IPv6
/// address.
fn expect_ipv6(ip: IpAddr) -> Ipv6Addr {
    let IpAddr::V6(ip) = ip else {
        panic!("Expected ipv6 address")
    };

    ip
}

impl left_right::Absorb<RoutingTableOplogEntry> for RoutingTableInner {
    fn absorb_first(&mut self, operation: &mut RoutingTableOplogEntry, _other: &Self) {
        match operation {
            RoutingTableOplogEntry::Upsert(subnet, list) => {
                self.table.insert(
                    expect_ipv6(subnet.address()),
                    subnet.prefix_len().into(),
                    Arc::clone(list),
                );
            }
            RoutingTableOplogEntry::Delete(subnet) => {
                self.table
                    .remove(expect_ipv6(subnet.address()), subnet.prefix_len().into());
            }
        }
    }

    fn sync_with(&mut self, first: &Self) {
        for (k, ss, v) in first.table.iter() {
            self.table.insert(k, ss, v.clone());
        }
    }

    fn absorb_second(&mut self, operation: RoutingTableOplogEntry, _: &Self) {
        match operation {
            RoutingTableOplogEntry::Upsert(subnet, list) => {
                self.table.insert(
                    expect_ipv6(subnet.address()),
                    subnet.prefix_len().into(),
                    list,
                );
            }
            RoutingTableOplogEntry::Delete(subnet) => {
                self.table
                    .remove(expect_ipv6(subnet.address()), subnet.prefix_len().into());
            }
        }
    }
}

impl Drop for RoutingTable {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}
