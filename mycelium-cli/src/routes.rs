use mycelium_api::{QueriedSubnet, Route};
use prettytable::{row, Table};
use std::path::Path;
use tracing::error;

use crate::rpc_client::rpc_call;

pub async fn list_selected_routes(
    socket_path: &Path,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = rpc_call(socket_path, "getSelectedRoutes", serde_json::json!({})).await?;

    if json_print {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let routes: Vec<Route> = serde_json::from_value(result).map_err(|e| {
            error!("Failed to parse selected routes response: {e}");
            e
        })?;
        let mut table = Table::new();
        table.add_row(row!["Subnet", "Next Hop", "Metric", "Seq No"]);
        for route in routes.iter() {
            table.add_row(row![
                &route.subnet,
                &route.next_hop,
                route.metric,
                route.seqno,
            ]);
        }
        table.printstd();
    }

    Ok(())
}

pub async fn list_fallback_routes(
    socket_path: &Path,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = rpc_call(socket_path, "getFallbackRoutes", serde_json::json!({})).await?;

    if json_print {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let routes: Vec<Route> = serde_json::from_value(result).map_err(|e| {
            error!("Failed to parse fallback routes response: {e}");
            e
        })?;
        let mut table = Table::new();
        table.add_row(row!["Subnet", "Next Hop", "Metric", "Seq No"]);
        for route in routes.iter() {
            table.add_row(row![
                &route.subnet,
                &route.next_hop,
                route.metric,
                route.seqno,
            ]);
        }
        table.printstd();
    }

    Ok(())
}

pub async fn list_queried_subnets(
    socket_path: &Path,
    json_print: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = rpc_call(socket_path, "getQueriedSubnets", serde_json::json!({})).await?;

    if json_print {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let queries: Vec<QueriedSubnet> = serde_json::from_value(result).map_err(|e| {
            error!("Failed to parse queried subnets response: {e}");
            e
        })?;
        let mut table = Table::new();
        table.add_row(row!["Subnet", "Query expiration"]);
        for query in queries.iter() {
            table.add_row(row![query.subnet, query.expiration,]);
        }
        table.printstd();
    }

    Ok(())
}
