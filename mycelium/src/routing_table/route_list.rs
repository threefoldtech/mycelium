use std::{
    ops::{Deref, DerefMut, Index, IndexMut},
    sync::Arc,
};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::{crypto::SharedSecret, peer::Peer, task::AbortHandle};

use super::{RouteEntry, RouteKey};

/// The RouteList holds all routes for a specific subnet.
// By convention, if a route is selected, it will always be at index 0 in the list.
#[derive(Clone)]
pub struct RouteList {
    list: Vec<(Arc<AbortHandle>, RouteEntry)>,
    shared_secret: SharedSecret,
}

impl RouteList {
    /// Create a new empty RouteList
    pub(crate) fn new(shared_secret: SharedSecret) -> Self {
        Self {
            list: Vec::new(),
            shared_secret,
        }
    }

    /// Returns the [`SharedSecret`] used for encryption of packets to and from the associated
    /// [`Subnet`].
    #[inline]
    pub fn shared_secret(&self) -> &SharedSecret {
        &self.shared_secret
    }

    /// Checks if there are any actual routes in the list.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Returns the selected route for the [`Subnet`] this is the `RouteList` for, if one exists.
    pub fn selected(&self) -> Option<&RouteEntry> {
        self.list
            .first()
            .map(|(_, re)| re)
            .and_then(|re| if re.selected() { Some(re) } else { None })
    }

    /// Returns an iterator over the `RouteList`.
    ///
    /// The iterator yields all [`route entries`](RouteEntry) in the list.
    pub fn iter(&self) -> RouteListIter {
        RouteListIter::new(self)
    }

    /// Returns an iterator over the `RouteList` yielding mutable access to the elements.
    ///
    /// The iterator yields all [`route entries`](RouteEntry) in the list.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = RouteGuard> {
        self.list.iter_mut().map(|item| RouteGuard { item })
    }

    /// Removes a [`RouteEntry`] from the `RouteList`.
    ///
    /// This does nothing if the neighbour does not exist.
    pub fn remove(&mut self, neighbour: &Peer) {
        let Some(pos) = self
            .list
            .iter()
            .position(|re| re.1.neighbour() == neighbour)
        else {
            return;
        };

        let old = self.list.swap_remove(pos);
        old.0.abort();
    }

    /// Swaps the position of 2 `RouteEntry`s in the route list.
    pub fn swap(&mut self, first: usize, second: usize) {
        self.list.swap(first, second)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut RouteEntry> {
        self.list.get_mut(index).map(|(_, re)| re)
    }

    /// Insert a new [`RouteEntry`] in the `RouteList`.
    pub fn insert(
        &mut self,
        re: RouteEntry,
        expired_route_entry_sink: mpsc::Sender<RouteKey>,
        cancellation_token: CancellationToken,
    ) {
        let expiration = re.expires();
        let rk = RouteKey::new(re.source().subnet(), re.neighbour().clone());
        let abort_handle = Arc::new(
                tokio::spawn(async move {
                    tokio::select! {
                        _ = cancellation_token.cancelled() => {}
                        _ = tokio::time::sleep_until(expiration) => {
                            debug!(route_key = %rk, "Expired route entry for route key");
                            if let Err(e) =  expired_route_entry_sink.send(rk).await {
                                error!(route_key = %e.0, "Failed to send expired route key on cleanup channel");
                            }
                        }
                    }
                })
                .abort_handle().into(),
            );

        self.list.push((abort_handle, re));
    }
}

pub struct RouteGuard<'a> {
    item: &'a mut (Arc<AbortHandle>, RouteEntry),
}

impl Deref for RouteGuard<'_> {
    type Target = RouteEntry;

    fn deref(&self) -> &Self::Target {
        &self.item.1
    }
}

impl DerefMut for RouteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.item.1
    }
}

impl RouteGuard<'_> {
    pub fn set_expires(
        &mut self,
        expires: tokio::time::Instant,
        expired_route_entry_sink: mpsc::Sender<RouteKey>,
        cancellation_token: CancellationToken,
    ) {
        let re = &mut self.item.1;
        re.set_expires(expires);
        let expiration = re.expires();
        let rk = RouteKey::new(re.source().subnet(), re.neighbour().clone());
        let abort_handle = Arc::new(
                tokio::spawn(async move {
                    tokio::select! {
                        _ = cancellation_token.cancelled() => {}
                        _ = tokio::time::sleep_until(expiration) => {
                            debug!(route_key = %rk, "Expired route entry for route key");
                            if let Err(e) =  expired_route_entry_sink.send(rk).await {
                                error!(route_key = %e.0, "Failed to send expired route key on cleanup channel");
                            }
                        }
                    }
                })
                .abort_handle().into(),
            );

        self.item.0.abort();
        self.item.0 = abort_handle;
    }
}

impl Index<usize> for RouteList {
    type Output = RouteEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.list[index].1
    }
}

impl IndexMut<usize> for RouteList {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.list[index].1
    }
}

pub struct RouteListIter<'a> {
    route_list: &'a RouteList,
    idx: usize,
}

impl<'a> RouteListIter<'a> {
    /// Create a new `RouteListIter` which will iterate over the given [`RouteList`].
    fn new(route_list: &'a RouteList) -> Self {
        Self { route_list, idx: 0 }
    }
}

impl<'a> Iterator for RouteListIter<'a> {
    type Item = &'a RouteEntry;

    fn next(&mut self) -> Option<Self::Item> {
        self.idx += 1;
        self.route_list.list.get(self.idx - 1).map(|(_, re)| re)
    }
}
