use std::{
    collections::HashMap,
    net::Ipv6Addr,
    sync::{Arc, RwLock},
};

use crate::{metrics::Metrics, router::Router};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    select,
    time::{self, timeout, Duration},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace};

/// Amount of time to wait between probes for proxies.
const PROXY_PROBE_INTERVAL: Duration = Duration::from_secs(60);
/// Default IANA assigned port for Socks proxies.
/// https://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.xhtml?search=1080
const DEFAULT_SOCKS5_PORT: u16 = 1080;
/// Amount of time before we consider a probe to be dropped.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Client greeting packet in the Socks5 protocol, starts the handshake.
const SOCKS5_CLIENT_GREETING: [u8; 3] = [
    0x05, // Version, 5 -> Socks5
    0x01, // NAUTH, number of authentication methods supported, only 1
    0x00, // AUTH, 1 byte per supported method, only 1 supported, 0 == No authentication
];
/// Server choice packet expected as reply to our client greeting while probing if a proxy is
/// present and open
const SOCKS5_EXPECTED_SERVER_CHOICE: [u8; 2] = [
    0x05, // Version, 5 -> Socks5
    0x00, // CAUTH, chosen authentication method, 0 since this is the only one offered
];
/// Server choice packet if the server denies your authentication
const SOCKS5_SERVER_CHOICE_DENIED: [u8; 2] = [
    0x05, // Version, 5 -> Socks5
    0xFF, // CAUTH, no acceptable methods
];

/// Proxy implementations scans known IPs from the [`Router`](crate::router::Router), to see if
/// there is an (open) SOCKS5 proxy listening on the default port on that IP.
pub struct Proxy<M> {
    router: Router<M>,
    proxy_cache: Arc<RwLock<HashMap<Ipv6Addr, ProxyProbeStatus>>>,
    cancel_token: CancellationToken,
}

/// Status of a probe for a proxy.
#[derive(Debug)]
pub enum ProxyProbeStatus {
    /// No process is listening on the probed port on the remote IP.
    NotListening,
    /// The last probe found a valid proxy server at this address, which does not require
    /// authentication.
    Valid,
    /// The last probe found a valid proxy server at this address, but it requires authentication
    AuthenticationRequired,
    /// A process is listening but it is not a valid Socks5 proxy
    NotAProxy,
}

impl<M> Proxy<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    /// Create a new `Proxy` implementation.
    pub fn new(router: Router<M>) -> Self {
        Self {
            router,
            proxy_cache: Arc::new(RwLock::new(HashMap::new())),
            cancel_token: CancellationToken::new(),
        }
    }

    /// Starst a background task which periodically scans the [`Router`](crate::router::Router)
    /// for potential new proxies.
    ///
    /// # Panics
    ///
    /// Panics if not called from the context of tokio runtime.
    pub fn start_probing(&self) {
        info!("Start Socks5 proxy probing");
        let router = self.router.clone();
        let proxy_cache = self.proxy_cache.clone();
        let cancel_token = self.cancel_token.clone();

        tokio::spawn(async move {
            loop {
                select! {
                 _ = time::sleep(PROXY_PROBE_INTERVAL) => {
                        debug!("Starting proxy probes");
                    },
                _ = cancel_token.cancelled() => {
                        break
                    }
                }

                let routes = router.load_selected_routes();

                for route in routes {
                    let proxy_cache = proxy_cache.clone();

                    tokio::spawn(async move {
                        let address = route.source().router_id().to_pubkey().address();

                        debug!(%address, "Probing Socks5 proxy");

                       if let Err(_) =  
                            timeout(PROBE_TIMEOUT, async {
                                let mut stream =
                                    match TcpStream::connect((address, DEFAULT_SOCKS5_PORT)).await {
                                    Ok(stream) => stream,
                                    Err(err) => {
                                        trace!(%address, %err, "Failed to connect");
                                            proxy_cache
                                                .write()
                                                .expect("Proxy cache can be write locked; qed")
                                                .insert(address, ProxyProbeStatus::NotListening);
                                        return
                                    }
                                };

                                trace!(%address, "Connection established");
                                if let Err(err) = stream.write_all(&SOCKS5_CLIENT_GREETING).await {
                                    trace!(%address, %err, "Failed to write greeting to remote");
                                    // If we can't write to the remote, assume nothing is listening
                                    // there.
                                    proxy_cache
                                        .write()
                                        .expect("Proxy cache can be write locked; qed")
                                        .insert(address, ProxyProbeStatus::NotListening);
                                    return
                                }
                                let mut recv_buf = [0; 2];
                                // We can use read exact here since we are wrapped in a timeout, so
                                // this eventually ends even if the remote does not send data.
                                if let Err(err) = stream.read_exact(&mut recv_buf).await {
                                    trace!(%address, %err, "Failed to read server reply from remote");
                                    // If we can't read from the remote, assume nothing is listening
                                    // there. (At least nothing valid)
                                    proxy_cache
                                        .write()
                                        .expect("Proxy cache can be write locked; qed")
                                        .insert(address, ProxyProbeStatus::NotListening);
                                    return
                                }

                                match recv_buf {
                                    SOCKS5_EXPECTED_SERVER_CHOICE => {
                                        debug!(%address, "Valid open Socks5 server found");
                                        proxy_cache
                                            .write()
                                            .expect("Proxy cache can be write locked; qed")
                                            .insert(address, ProxyProbeStatus::Valid);
                                    }
                                    SOCKS5_SERVER_CHOICE_DENIED => {
                                        debug!(%address, "Valid Socks5 server found, but it requires authentication");
                                        proxy_cache
                                            .write()
                                            .expect("Proxy cache can be write locked; qed")
                                            .insert(address, ProxyProbeStatus::AuthenticationRequired);
                                    }
                                    _ => {
                                        debug!(%address, "Reply does not match the expected reply format of a Socks5 proxy");
                                        proxy_cache
                                            .write()
                                            .expect("Proxy cache can be write locked; qed")
                                            .insert(address, ProxyProbeStatus::NotAProxy);
                                    }
                                }
                            })
                            .await {
                                debug!(%address, "Timeout probing proxy");
                                proxy_cache
                                    .write()
                                    .expect("Proxy cache can be write locked; qed")
                                    .insert(address, ProxyProbeStatus::NotListening);
                        }
                    });
                }
            }
        });
    }

    /// Stops any ongoing probes. 
    pub fn stop_scanning(&self) {
        info!("Stopping Socks5 proxy probing");
        self.cancel_token.cancel();
    }
}
