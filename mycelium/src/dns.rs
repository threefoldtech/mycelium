//! Built in dns resolver which intercepts dns lookups for defined TLD's and redirects them to a
//! different upstream server.

use hickory_server::authority::MessageResponseBuilder;
use hickory_server::proto::op::Header;
use hickory_server::proto::rr::{LowerName, Record};
use hickory_server::server::{Request, RequestHandler, ResponseHandler, ResponseInfo};
use hickory_server::ServerFuture;
use tokio::net::UdpSocket;
use tracing::{debug, error, info};

pub struct Resolver {
    server: ServerFuture<Handler>,
}

pub struct Handler {
    _intercepted_zones: Vec<LowerName>,
    resolver: hickory_resolver::TokioResolver,
}

impl Resolver {
    /// Create a new resolver instance
    pub async fn new() -> Self {
        let resolver = hickory_resolver::TokioResolver::builder_tokio()
            .expect("Can create tokio resolver builder")
            .build();
        let handler = Handler {
            _intercepted_zones: Vec::new(),
            resolver,
        };

        let mut server = hickory_server::server::ServerFuture::new(handler);
        let udp_socket = UdpSocket::bind("[::]:53")
            .await
            .expect("Can bind udp port 53");
        server.register_socket(udp_socket);

        Self { server }
    }
}

#[async_trait::async_trait]
impl RequestHandler for Handler {
    #[tracing::instrument(skip_all, Level = DEBUG)]
    async fn handle_request<R>(&self, request: &Request, mut response_handle: R) -> ResponseInfo
    where
        R: ResponseHandler,
    {
        let mut answers = vec![];
        // We only handle queries. Anything which isn't a query shouldn't be targeted at us anyway,
        // since we only act as a (recursive) resolver
        for query in request.queries() {
            // Check if this is a query we want to redirect
            // NOTE: don't use query.query_type.is_ip_addr() since that matches requests for both A
            // and AAAA records, while we obviously only care about AAAA records.
            // if query.query_class() == DNSClass::IN && query.query_type() == RecordType::AAAA {
            //     let mut intercepted = false;
            //     for zone in &self.intercepted_zones {
            //         if zone.zone_of(query.name()) {
            //             intercepted = true;
            //             // TODO: fetch answer
            //             todo!();
            //         }
            //     }
            //     if !intercepted {
            //         // Send to system upstream
            //         let res = self.resolver.ipv6_lookup(query.name()).await;
            //         match res {
            //             Ok(record) => {}
            //             Err(err) => {
            //                 debug!(%err, domain = %query.name(), "IPv6 record lookup failed");
            //                 let resp = MessageResponseBuilder::from_message_request(request)
            //                     .error_msg(
            //                         &Header::response_from_request(request.header()),
            //                         ResponseCode::ServFail,
            //                     );
            //                 if let Err(err) = response_handle.send_response(resp).await {
            //                     debug!(%err, "Failed to send response");
            //                 }
            //             }
            //         }
            //     }
            // } else {
            //     // Send to system upstream
            //     todo!();
            // }

            match self.resolver.lookup(query.name(), query.query_type()).await {
                Ok(lookup) => {
                    answers.push(lookup);
                }
                Err(err) => {
                    error!(%err, name = %query.name(), class = %query.query_type(), "Could not resolve query");
                }
            }
        }

        let answers = answers
            .into_iter()
            .flat_map(|lookup| {
                let ttl = lookup
                    .valid_until()
                    .duration_since(std::time::Instant::now())
                    .as_secs() as u32;
                let name = lookup.query().name().clone();
                lookup
                    .into_iter()
                    .zip(std::iter::repeat(name))
                    .map(move |(rdata, name)| {
                        info!(%name, record = %rdata, "Resolution done");
                        Record::from_rdata(name, ttl, rdata)
                    })
            })
            .collect::<Vec<_>>();

        let responses = MessageResponseBuilder::from_message_request(request);
        let resp = responses.build(
            Header::response_from_request(request.header()),
            &answers,
            [],
            [],
            [],
        );

        match response_handle.send_response(resp).await {
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
        self.server.shutdown_token().cancel()
    }
}
