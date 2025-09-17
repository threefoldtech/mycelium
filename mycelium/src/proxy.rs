use std::{
    collections::HashMap,
    net::{Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex, RwLock},
};

use crate::{metrics::Metrics, router::Router};
use futures::stream::FuturesUnordered;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
    time::{self, timeout, Duration},
};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

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
    chosen_remote: Arc<Mutex<Option<SocketAddr>>>,
    /// Cancellation token used for scanning routines
    scan_token: Arc<Mutex<CancellationToken>>,
    /// Cancellation token used for the actual proxy connections
    proxy_token: Arc<Mutex<CancellationToken>>,
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
            chosen_remote: Arc::new(Mutex::new(None)),
            scan_token: Arc::new(Mutex::new(CancellationToken::new())),
            proxy_token: Arc::new(Mutex::new(CancellationToken::new())),
        }
    }

    /// Get a list of all known proxies we discovered.
    pub fn known_proxies(&self) -> Vec<Ipv6Addr> {
        self.proxy_cache
            .read()
            .expect("Can read lock proxy cache; qed")
            .iter()
            .filter_map(|(addr, s)| {
                if matches!(s, ProxyProbeStatus::Valid) {
                    Some(*addr)
                } else {
                    None
                }
            })
            .collect()
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
        let cancel_token = self
            .scan_token
            .lock()
            .expect("Can lock cancel_token; qed")
            .clone();

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

                        if timeout(PROBE_TIMEOUT, async {
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
                            .await.is_err() {
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
    pub fn stop_probing(&self) {
        info!("Stopping Socks5 proxy probing");
        self.scan_token
            .lock()
            .expect("Can lock cancel token; qed")
            .cancel();
    }

    /// Connect to a remote Socks5 proxy. If a proxy address is given, connect to that one. If not, connect to the best (fastest) known proxy.
    pub async fn connect(&self, remote: Option<SocketAddr>) -> Result<SocketAddr, ConnectionError> {
        let target = match remote {
            Some(remote) => remote,
            None => {
                debug!("Finding best known proxy");
                // Find best proxy of our internal list by racing all proxies and finding the first
                // one which gives a valid response.
                let futs = FuturesUnordered::new();
                for ip in self
                    .proxy_cache
                    .read()
                    .expect("Can read lock proxy cache; qed")
                    .iter()
                    .filter_map(|(address, ps)| {
                        if matches!(ps, ProxyProbeStatus::Valid) {
                            Some(*address)
                        } else {
                            None
                        }
                    })
                {
                    futs.push(async move {
                        // It's fine to swallow errors here, we are just sanity checking
                        let addr: SocketAddr = (ip, DEFAULT_SOCKS5_PORT).into();
                        trace!(%addr, "Checking proxy availability and latency");
                        let mut stream = TcpStream::connect(addr).await.ok()?;
                        stream.write_all(&SOCKS5_CLIENT_GREETING).await.ok()?;
                        let mut recv_buf = [0; 2];
                        stream.read_exact(&mut recv_buf).await.ok()?;
                        match recv_buf {
                            SOCKS5_EXPECTED_SERVER_CHOICE => Some(addr),
                            _ => None,
                        }
                    });
                }

                let target: Option<SocketAddr> = futs.filter_map(|o| o).next().await;
                if target.is_none() {
                    return Err(ConnectionError { _private: () });
                }
                // Safe since we just checked the None case above
                target.unwrap()
            }
        };

        // Now that we have a target, "connect" to it, i.e. set it as proxy destination.
        debug!(%target, "Setting remote Socks5 proxy");
        *self
            .chosen_remote
            .lock()
            .expect("Can lock chosen remote; qed") = Some(target);

        self.start_proxy();

        Ok(target)
    }

    /// Disconnects from the proxy, if any is connected
    pub fn disconnect(&self) {
        self.proxy_token
            .lock()
            .expect("Can lock proxy token; qed")
            .cancel();
        *self
            .chosen_remote
            .lock()
            .expect("Can lock chosen remote; qed") = None;
    }

    /// Starts a background task for proxying connections.
    /// This spawns a listener, and proxies all connections to the chosen target.
    fn start_proxy(&self) {
        let target = *self
            .chosen_remote
            .lock()
            .expect("Can lock chosen remote; qed");
        // First cancel the old token, then set a new token
        let mut old_token = self.proxy_token.lock().expect("Can lock proxy token; qed");
        old_token.cancel();
        let proxy_token = CancellationToken::new();
        *old_token = proxy_token.clone();

        tokio::spawn(async move {
            let listener = TcpListener::bind(("[::]", DEFAULT_SOCKS5_PORT)).await?;

            loop {
                select! {
                    _ = proxy_token.cancelled() => {
                        debug!("Shutting down proxy listener");
                        break
                    }
                    stream = listener.accept() => {
                        match stream {
                            Err(err) => {
                                error!(%err, "Proxy listener accept error");
                                return Err(err)
                           }
                            Ok((mut stream, source)) => {
                                trace!(%source, "Got new proxy stream");
                                let proxy_token = proxy_token.clone();
                                let target = target;
                                if target.is_none() {
                                    warn!("Refusing to proxy Socks5 stream as we have no remote set - this should not happen");
                                    break
                                }
                                // Unwrap is safe since we checked the none variant above
                                let target = target.unwrap();
                               tokio::spawn(async move {
                                    let mut con = TcpStream::connect(target).await?;
                                    select! {
                                        _ = proxy_token.cancelled() => {
                                            trace!(%source, %target, "Shutting down proxy stream");
                                            Ok(())
                                        }
                                        r = tokio::io::copy_bidirectional(&mut stream, &mut con) => {
                                            match r {
                                                Err(err) => {
                                                    trace!(%err, %source, %target, "Proxy stream finished with error");
                                                    Err(err)
                                                }
                                                Ok((_, _)) => {
                                                    trace!(%source, %target, "Proxy stream finished normally");
                                                    Ok(())
                                                }
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    }
                }
            }

            Ok(())
        });
    }
}

/// Error returned when performing an automatic Socks5 connect, but no valid remotes are found.
#[derive(Debug)]
pub struct ConnectionError {
    _private: (),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("No valid Socks5 proxy found to connect to")
    }
}

impl std::error::Error for ConnectionError {}
