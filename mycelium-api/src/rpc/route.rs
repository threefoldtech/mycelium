//! Route-related JSON-RPC methods for the Mycelium API

use jsonrpc_core::Result as RpcResult;
use tracing::debug;

use mycelium::metrics::Metrics;

use crate::HttpServerState;
use crate::Route;
use crate::QueriedSubnet;
use crate::NoRouteSubnet;
use crate::Metric;
use crate::rpc::traits::RouteApi;

/// Implementation of Route-related JSON-RPC methods
pub struct RouteRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    state: HttpServerState<M>,
}

impl<M> RouteRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    /// Create a new RouteRpc instance
    pub fn new(state: HttpServerState<M>) -> Self {
        Self { state }
    }
}

impl<M> RouteApi for RouteRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    fn get_selected_routes(&self) -> RpcResult<Vec<Route>> {
        debug!("Loading selected routes via RPC");
        let routes = self.state
            .node
            .blocking_lock()
            .selected_routes()
            .into_iter()
            .map(|sr| Route {
                subnet: sr.source().subnet().to_string(),
                next_hop: sr.neighbour().connection_identifier().clone(),
                metric: if sr.metric().is_infinite() {
                    Metric::Infinite
                } else {
                    Metric::Value(sr.metric().into())
                },
                seqno: sr.seqno().into(),
            })
            .collect();
        
        Ok(routes)
    }
    
    fn get_fallback_routes(&self) -> RpcResult<Vec<Route>> {
        debug!("Loading fallback routes via RPC");
        let routes = self.state
            .node
            .blocking_lock()
            .fallback_routes()
            .into_iter()
            .map(|sr| Route {
                subnet: sr.source().subnet().to_string(),
                next_hop: sr.neighbour().connection_identifier().clone(),
                metric: if sr.metric().is_infinite() {
                    Metric::Infinite
                } else {
                    Metric::Value(sr.metric().into())
                },
                seqno: sr.seqno().into(),
            })
            .collect();
        
        Ok(routes)
    }
    
    fn get_queried_subnets(&self) -> RpcResult<Vec<QueriedSubnet>> {
        debug!("Loading queried subnets via RPC");
        let queries = self.state
            .node
            .blocking_lock()
            .queried_subnets()
            .into_iter()
            .map(|qs| QueriedSubnet {
                subnet: qs.subnet().to_string(),
                expiration: qs
                    .query_expires()
                    .duration_since(tokio::time::Instant::now())
                    .as_secs()
                    .to_string(),
            })
            .collect();
        
        Ok(queries)
    }
    
    fn get_no_route_entries(&self) -> RpcResult<Vec<NoRouteSubnet>> {
        debug!("Loading no route entries via RPC");
        let entries = self.state
            .node
            .blocking_lock()
            .no_route_entries()
            .into_iter()
            .map(|nrs| NoRouteSubnet {
                subnet: nrs.subnet().to_string(),
                expiration: nrs
                    .entry_expires()
                    .duration_since(tokio::time::Instant::now())
                    .as_secs()
                    .to_string(),
            })
            .collect();
        
        Ok(entries)
    }
}