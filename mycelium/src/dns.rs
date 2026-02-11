//! Built in dns resolver which forwards DNS queries to the public mycelium node with the lowest
//! route metric, falling back to 1.1.1.1 via hickory-resolver if no routes are available.

use crate::metric::Metric;
use crate::metrics::Metrics;
use crate::router::{RouteStatus, Router};
use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::rr::{Record, RecordType};
use hickory_resolver::ResolveError;
use hickory_server::authority::MessageResponseBuilder;
use hickory_server::proto::op::{Header, Message, MessageType, OpCode};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::ServerFuture;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Public mycelium nodes to consider for DNS forwarding.
const PUBLIC_DNS_NODES: [Ipv6Addr; 10] = [
    // DE
    Ipv6Addr::new(
        0x054b, 0x83ab, 0x6cb5, 0x7b38, 0x44ae, 0xcd14, 0x53f3, 0xa907,
    ),
    Ipv6Addr::new(
        0x040a, 0x152c, 0xb85b, 0x9646, 0x5b71, 0xd03a, 0xeb27, 0x2462,
    ),
    // BE
    Ipv6Addr::new(
        0x0597, 0xa4ef, 0x0806, 0x0b09, 0x6650, 0xcbbf, 0x1b68, 0xcc94,
    ),
    Ipv6Addr::new(
        0x0549, 0x8bce, 0xfa45, 0xe001, 0xcbf8, 0xf2e2, 0x2da6, 0xa67c,
    ),
    // FI
    Ipv6Addr::new(
        0x0410, 0x2778, 0x53bf, 0x6f41, 0xaf28, 0x1b60, 0xd7c0, 0x707a,
    ),
    Ipv6Addr::new(
        0x0488, 0x74ac, 0x8a31, 0x277b, 0x9683, 0x0c8e, 0xe14f, 0x79a7,
    ),
    // US-EAST
    Ipv6Addr::new(
        0x04ab, 0xa385, 0x5a4e, 0xef8f, 0x92e0, 0x1605, 0x7cb6, 0x24b2,
    ),
    // US-WEST
    Ipv6Addr::new(
        0x04de, 0xb695, 0x3859, 0x8234, 0xd04c, 0x5de6, 0x8097, 0xc27c,
    ),
    // SG
    Ipv6Addr::new(
        0x05eb, 0xc711, 0xf9ab, 0xeb24, 0xff26, 0xe392, 0xa115, 0x1c0e,
    ),
    // IND
    Ipv6Addr::new(
        0x0445, 0x0465, 0xfe81, 0x1e2b, 0x5420, 0xa029, 0x06b0, 0x9f61,
    ),
];

const FALLBACK_DNS: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);
const FALLBACK_NS: [Ipv4Addr; 2] = [FALLBACK_DNS, Ipv4Addr::new(1, 0, 0, 1)];

/// Interval between metric checks for public DNS nodes.
const METRIC_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// DNS port.
const DNS_PORT: u16 = 53;

/// Timeout for DNS forwarding requests.
const DNS_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Resolver {
    server: ServerFuture<Handler>,
    cancel_token: CancellationToken,
}

struct Handler {
    best_node: Arc<RwLock<Option<Ipv6Addr>>>,
    fallback_resolver: hickory_resolver::Resolver<TokioConnectionProvider>,
}

impl Resolver {
    /// Create a new resolver instance with the given router.
    pub async fn new<M>(router: Router<M>) -> Self
    where
        M: Metrics + Clone + Send + Sync + 'static,
    {
        let best_node = Arc::new(RwLock::new(None));
        let cancel_token = CancellationToken::new();

        // Start background metric checker - router is moved into the task
        start_metric_checker(router, best_node.clone(), cancel_token.clone());

        let nameserver_group =
            NameServerConfigGroup::from_ips_clear(&FALLBACK_NS.map(IpAddr::V4), DNS_PORT, true);
        let mut fallback_opts = ResolverOpts::default();
        fallback_opts.timeout = Duration::from_secs(2);
        let fallback_config = ResolverConfig::from_parts(None, vec![], nameserver_group);
        let fallback_resolver = hickory_resolver::Resolver::builder_with_config(
            fallback_config,
            TokioConnectionProvider::default(),
        )
        .with_options(fallback_opts)
        .build();

        let handler = Handler {
            best_node,
            fallback_resolver,
        };

        let mut server = hickory_server::server::ServerFuture::new(handler);
        let udp_socket = UdpSocket::bind("[::]:53")
            .await
            .expect("Can bind udp port 53");
        server.register_socket(udp_socket);

        Self {
            server,
            cancel_token,
        }
    }
}

/// Start a background task that periodically checks route metrics for public DNS nodes.
fn start_metric_checker<M>(
    router: Router<M>,
    best_node: Arc<RwLock<Option<Ipv6Addr>>>,
    cancel_token: CancellationToken,
) where
    M: Metrics + Clone + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(METRIC_CHECK_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    update_best_node(&router, &best_node);
                }
                _ = cancel_token.cancelled() => {
                    debug!("DNS metric checker shutting down");
                    break;
                }
            }
        }
    });
}

/// Update the best DNS node based on current route metrics.
fn update_best_node<M>(router: &Router<M>, best_node: &Arc<RwLock<Option<Ipv6Addr>>>)
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let mut best: Option<(Ipv6Addr, Metric)> = None;

    for node_ip in PUBLIC_DNS_NODES {
        match router.route_status(IpAddr::V6(node_ip)) {
            RouteStatus::Selected(metric) => {
                if !metric.is_infinite() {
                    match &best {
                        None => best = Some((node_ip, metric)),
                        Some((_, best_metric)) if metric < *best_metric => {
                            best = Some((node_ip, metric));
                        }
                        _ => {}
                    }
                }
            }
            RouteStatus::NoRoute => {
                // Destination is known unreachable, don't retry
            }
            RouteStatus::Queried => {
                // Route request already in progress, wait for next tick
            }
            RouteStatus::Unknown => {
                // No info - trigger a route request for next time
                router.request_route(IpAddr::V6(node_ip));
            }
            RouteStatus::Fallback => {
                // Has routes but none selected - not usable for DNS forwarding
            }
        }
    }

    let new_best = best.map(|(ip, _)| ip);
    *best_node.write().expect("Can write lock best_node; qed") = new_best;

    if let Some(ip) = new_best {
        debug!(%ip, "Updated best DNS node");
    } else {
        debug!("No route to any public DNS node, will use fallback");
    }
}

impl Handler {
    /// Forward A/AAAA via hickory-resolver (1.1.1.1). Used when no overlay routes exist.
    async fn forward_dns_fallback(
        &self,
        request: &Request,
    ) -> Result<Vec<Record>, DnsForwardError> {
        let query = request.queries().iter().next().ok_or_else(|| {
            DnsForwardError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "No query in request",
            ))
        })?;
        let name = query.original().name().to_string();
        let record_type = query.original().query_type();

        if record_type != RecordType::A && record_type != RecordType::AAAA {
            return Err(DnsForwardError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("Fallback only supports A/AAAA, got {:?}", record_type),
            )));
        }

        let lookup = tokio::time::timeout(
            Duration::from_secs(2),
            self.fallback_resolver.lookup_ip(name.as_str()),
        )
        .await
        .map_err(|_| DnsForwardError::Timeout)?
        .map_err(DnsForwardError::Resolve)?;

        Ok(lookup.as_lookup().records().to_vec())
    }

    /// Forward a DNS request to a mycelium node via raw UDP.
    async fn forward_dns(
        &self,
        request: &Request,
        target_ip: IpAddr,
    ) -> Result<Message, DnsForwardError> {
        let socket = UdpSocket::bind("[::]:0")
            .await
            .map_err(DnsForwardError::Io)?;
        let target_addr = SocketAddr::new(target_ip, DNS_PORT);

        // Build the DNS query message
        let mut message = Message::new();
        message.set_id(request.id());
        message.set_message_type(MessageType::Query);
        message.set_op_code(OpCode::Query);
        message.set_recursion_desired(true);
        for query in request.queries() {
            message.add_query(query.original().clone());
        }

        let request_bytes = message.to_vec().map_err(DnsForwardError::Proto)?;

        socket
            .send_to(&request_bytes, target_addr)
            .await
            .map_err(DnsForwardError::Io)?;

        let mut buf = [0u8; 4096];
        match tokio::time::timeout(DNS_TIMEOUT, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, _))) => Message::from_vec(&buf[..len]).map_err(DnsForwardError::Proto),
            Ok(Err(e)) => Err(DnsForwardError::Io(e)),
            Err(_) => Err(DnsForwardError::Timeout),
        }
    }
}

#[async_trait::async_trait]
impl RequestHandler for Handler {
    #[tracing::instrument(skip_all, Level = DEBUG)]
    async fn handle_request<R>(&self, request: &Request, mut response_handle: R) -> ResponseInfo
    where
        R: ResponseHandler,
    {
        let best = *self.best_node.read().expect("Can read lock best_node; qed");
        let target_ip: IpAddr = best.map(IpAddr::V6).unwrap_or(FALLBACK_DNS.into());

        let responses = MessageResponseBuilder::from_message_request(request);
        let header = Header::response_from_request(request.header());
        let resp = {
            let result = if target_ip == IpAddr::V4(FALLBACK_DNS) {
                self.forward_dns_fallback(request).await
            } else {
                match self.forward_dns(request, target_ip).await {
                    Ok(msg) => Ok(msg.answers().to_vec()),
                    Err(e) => {
                        warn!(%e, %target_ip, "Overlay DNS failed, trying fallback");
                        self.forward_dns_fallback(request).await
                    }
                }
            };
            match result {
                Ok(answers) => {
                    info!(%target_ip, "DNS forward successful");
                    let resp = responses.build(header, answers.iter(), [], [], []);
                    response_handle.send_response(resp).await
                }
                Err(e) => {
                    warn!(%e, %target_ip, "Mycelium DNS forward failed");
                    let resp = responses
                        .error_msg(&header, hickory_server::proto::op::ResponseCode::ServFail);
                    response_handle.send_response(resp).await
                }
            }
        };

        match resp {
            Ok(resp_info) => resp_info,
            Err(err) => {
                debug!(%err, "Failed to send response");
                Header::response_from_request(request.header()).into()
            }
        }
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        self.cancel_token.cancel();
        self.server.shutdown_token().cancel();
    }
}

/// Error type for DNS forwarding operations.
#[derive(Debug)]
enum DnsForwardError {
    Io(std::io::Error),
    Proto(hickory_server::proto::ProtoError),
    Resolve(ResolveError),
    Timeout,
}

impl std::fmt::Display for DnsForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::Proto(e) => write!(f, "Protocol error: {}", e),
            Self::Resolve(e) => write!(f, "Resolve error: {}", e),
            Self::Timeout => write!(f, "DNS request timed out"),
        }
    }
}

impl std::error::Error for DnsForwardError {}
