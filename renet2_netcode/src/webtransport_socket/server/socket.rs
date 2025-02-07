use anyhow::Error;
use bytes::Bytes;
use log::{debug, error, trace};
use quinn::crypto::rustls::QuicServerConfig;
use quinn::IdleTimeout;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::{sync::mpsc, task::AbortHandle};
use wtransport::error::SendDatagramError;

use std::collections::HashMap;
use std::ops::Bound::{Excluded, Included};
use std::sync::atomic::AtomicU64;
use std::{
    collections::{BTreeMap, HashSet},
    io::ErrorKind,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
    vec,
};

use crate::{
    client_idx_from_addr, client_idx_to_addr, NetcodeTransportError, ServerCertHash, ServerSocket, WebServerDestination, HTTP_CONNECT_REQ,
};

use super::{generate_self_signed_certificate_opinionated, get_server_cert_hash};

/// Configuration for setting up a [`WebTransportServer`].
#[derive(Debug)]
pub struct WebTransportServerConfig {
    /// The certificate for this server.
    ///
    /// Note that if the certificate expires, then the server will no longer make connections.
    /// This is relevant for clients that use [`ServerCertHash`], which can only connect to certificates with an
    /// expiration under
    /// [two weeks](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#servercertificatehashes).
    pub cert: CertificateDer<'static>,
    /// The private key for this server.
    pub key: PrivateKeyDer<'static>,
    /// Socket address to listen on.
    ///
    /// It is recommended to use a pre-defined IP and a wildcard port.
    /// The pre-defined IP should be used when obtaining [`Self::cert`] from your certificate authority (CA).
    ///
    /// Using a wildcard port will reduce your chance of competing with other sockets on your machine (e.g. other
    /// WebTransport servers running different game instances).
    pub listen: SocketAddr,
    /// Maximum number of active clients allowed.
    pub max_clients: usize,
    //todo: client keep-alive timeout
}

impl WebTransportServerConfig {
    /// Makes a new config with a self-signed [`CertificateDer`] tied to the `listen` address.
    ///
    /// Returns the [`ServerCertHash`] of the certificate, which can be used to set up clients via
    /// `WebTransportClientConfig`.
    ///
    /// The certificate produced will be valid for two weeks (minus one hour and one minute).
    ///
    /// Use [`Self::new_selfsigned_with_proxies`] if you want the certificate to bind to a URL (e.g. domain name).
    pub fn new_selfsigned(listen: SocketAddr, max_clients: usize) -> Result<(Self, ServerCertHash), Error> {
        Self::new_selfsigned_with_proxies(listen, vec![listen.into()], max_clients)
    }

    /// Makes a new config with a self-signed [`CertificateDer`] tied to the `proxies` destinations.
    ///
    /// Returns the [`ServerCertHash`] of the certificate, which can be used to set up clients via
    /// `WebTransportClientConfig`.
    ///
    /// The certificate produced will be valid for two weeks (minus one hour and one minute).
    ///
    /// Use [`Self::new_selfsigned`] if you only want the certificate to bind to the `listen` address.
    pub fn new_selfsigned_with_proxies(
        listen: SocketAddr,
        proxies: Vec<WebServerDestination>,
        max_clients: usize,
    ) -> Result<(Self, ServerCertHash), Error> {
        let (cert, key) = generate_self_signed_certificate_opinionated(proxies)?;
        let hash = get_server_cert_hash(&cert);
        let config = WebTransportServerConfig {
            cert,
            key,
            listen,
            max_clients,
        };

        Ok((config, hash))
    }

    /// Converts self into a [`wtransport::ServerConfig`].
    ///
    /// Used automatically by [`WebTransportServer::new`].
    pub fn create_server_config(self) -> Result<wtransport::ServerConfig, Error> {
        // TODO: Allow injecting cert resolver via `with_cert_resolver()`, which would allow more than one certificate.
        // That would be useful for long-lived servers whose clients are using ServerCertHash, since then you could
        // specify many certificates (for the expected lifetime of the server) or even inject fresh ones via atomics
        // and channels.
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let mut tls_config = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(vec![self.cert], self.key)?;

        tls_config.max_early_data_size = u32::MAX;
        // We set the ALPN protocols to h3 as first, so that the browser will use the newest HTTP/3 draft and as fallback
        // we use older versions of the HTTP/3 draft.
        let alpn: Vec<Vec<u8>> = vec![
            b"h3".to_vec(),
            b"h3-32".to_vec(),
            b"h3-31".to_vec(),
            b"h3-30".to_vec(),
            b"h3-29".to_vec(),
        ];
        tls_config.alpn_protocols = alpn;

        let mut server_config: quinn::ServerConfig = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls_config)?));
        let mut transport_config = quinn::TransportConfig::default();
        transport_config
            .keep_alive_interval(Some(Duration::from_secs(2)))
            .max_idle_timeout(Some(IdleTimeout::try_from(Duration::from_secs(15))?));
        server_config.transport = Arc::new(transport_config);

        let wt_config = wtransport::ServerConfig::builder()
            .with_bind_address(self.listen)
            .build_with_quic_config(server_config);

        Ok(wt_config)
    }
}

impl Clone for WebTransportServerConfig {
    fn clone(&self) -> Self {
        Self {
            cert: self.cert.clone(),
            key: self.key.clone_key(),
            listen: self.listen,
            max_clients: self.max_clients,
        }
    }
}

/// A WT client tracked by the server.
struct WebTransportServerClient {
    /// Connection session.
    ///
    // TODO: When this is dropped, is a close message send to the client?
    session: wtransport::Connection,
    reader_receiver: crossbeam::channel::Receiver<Bytes>,
    abort_sender: mpsc::UnboundedSender<()>,
    /// When this struct is dropped, the reader thread will shut down automatically since the `abort_sender` channel
    /// will close.
    reader_thread: tokio::task::JoinHandle<()>,
    /// Netcode client id.
    client_id: u64,
}

/// Wrapper struct for communicating connection requests from the internal connection handler to the server.
struct ConnectionRequest {
    client_idx: u64,
    packet: Vec<u8>,
    result_sender: mpsc::Sender<ConnectionRequestResult>,
}

enum ConnectionRequestResult {
    Success { client_id: u64 },
    Failure,
}

/// Represents a client that is pending in the internal connection handler.
struct PendingClient {
    client_idx: u64,
    client_id: Option<u64>,
    result_sender: mpsc::Sender<ConnectionRequestResult>,
    buffered_response: Option<Bytes>,
}

impl PendingClient {
    fn new(client_idx: u64, result_sender: mpsc::Sender<ConnectionRequestResult>) -> Self {
        Self {
            client_idx,
            client_id: None,
            result_sender,
            buffered_response: None,
        }
    }

    /// Sets the buffered response with the first packet received.
    fn set_buffer(&mut self, packet: &[u8]) {
        if self.buffered_response.is_some() {
            return;
        }
        self.buffered_response = Some(Bytes::copy_from_slice(packet));
    }
}

/// Wrapper enum for client sessions passed from the internal connection handler to the server.
enum ClientConnectionResult {
    Success {
        client_idx: u64,
        client_id: u64,
        session: wtransport::Connection,
    },
    Failure {
        client_idx: u64,
    },
}

/// Implementation of [`ServerSocket`] for WebTransport servers.
///
/// The server handles connections internally with `tokio`.
pub struct WebTransportServer {
    handle: tokio::runtime::Handle,

    addr: SocketAddr,

    connection_req_receiver: mpsc::Receiver<ConnectionRequest>,
    connection_receiver: mpsc::Receiver<ClientConnectionResult>,
    connection_abort_handle: AbortHandle,

    client_iterator: Arc<AtomicU64>,
    pending_clients: HashMap<u64, PendingClient>,
    clients: BTreeMap<u64, WebTransportServerClient>,
    /// Maps netcode client ids to internal client indices.
    client_id_to_idx: HashMap<u64, u64>,
    lost_clients: HashSet<u64>,

    closed: bool,
    current_clients: Arc<AtomicUsize>,
    recv_index: u64,
}

impl WebTransportServer {
    /// Makes a new server.
    ///
    /// ## Errors
    /// - Errors if unable to build a `rustls::ServerConfig`.
    /// - Errors if unable to bind to the [`WebTransportServerConfig::listen`] address, which can happen if your
    ///   machine is using all ports on a pre-defined IP address.
    pub fn new(config: WebTransportServerConfig, handle: tokio::runtime::Handle) -> Result<Self, Error> {
        let max_clients = config.max_clients;
        let server_config = config.create_server_config()?;
        let endpoint = handle.block_on(async move { wtransport::Endpoint::server(server_config) })?;
        let addr = endpoint.local_addr()?;
        let (sender, receiver) = mpsc::channel::<ClientConnectionResult>(max_clients);
        let client_iterator = Arc::new(AtomicU64::new(0));
        let current_clients = Arc::new(AtomicUsize::new(0));
        let (connection_req_sender, connection_req_receiver) = mpsc::channel::<ConnectionRequest>(max_clients);
        let abort_handle = handle
            .spawn(Self::accept_connection(
                sender,
                endpoint,
                client_iterator.clone(),
                Arc::clone(&current_clients),
                connection_req_sender,
                max_clients,
            ))
            .abort_handle();

        Ok(Self {
            handle,
            addr,
            connection_req_receiver,
            connection_receiver: receiver,
            connection_abort_handle: abort_handle,
            client_iterator,
            pending_clients: HashMap::default(),
            clients: BTreeMap::new(),
            client_id_to_idx: HashMap::default(),
            lost_clients: HashSet::new(),
            closed: false,
            current_clients,
            recv_index: 0,
        })
    }

    /// Disconnects the server.
    // TODO: verify that aborting the endpoint's thread is enough to shut it down properly
    pub fn close(&mut self) {
        self.connection_abort_handle.abort();
        self.closed = true;
    }

    async fn accept_connection(
        sender: mpsc::Sender<ClientConnectionResult>,
        endpoint: wtransport::Endpoint<wtransport::endpoint::endpoint_side::Server>,
        client_iterator: Arc<AtomicU64>,
        current_clients: Arc<AtomicUsize>,
        connection_req_sender: mpsc::Sender<ConnectionRequest>,
        max_clients: usize,
    ) {
        loop {
            let incoming_connection = endpoint.accept().await;

            // Check for capacity.
            let is_full = {
                let current_clients = current_clients.load(Ordering::Relaxed);
                // We allow 25% extra clients in case clients want to override their old sessions.
                (current_clients * 4) >= (max_clients * 5)
            };
            if is_full {
                incoming_connection.refuse();
                continue;
            }

            let sender = sender.clone();
            let client_iterator = client_iterator.clone();
            let connection_req_sender = connection_req_sender.clone();
            tokio::spawn(async move {
                match incoming_connection.await {
                    Ok(session_request) => {
                        match Self::handle_session_request(client_iterator, connection_req_sender, session_request).await {
                            Ok(maybe_session) => {
                                if let Some(session) = maybe_session {
                                    if let Err(e) = sender.try_send(session) {
                                        debug!("Failed to send session to main thread: {e}");
                                    }
                                }
                            }
                            Err(err) => {
                                debug!("Failed to handle connection: {err:?}");
                            }
                        }
                    }
                    Err(err) => {
                        debug!("accepting connection failed: {err:?}");
                    }
                }
            });
        }
    }

    async fn handle_session_request(
        client_iterator: Arc<AtomicU64>,
        connection_req_sender: mpsc::Sender<ConnectionRequest>,
        session_request: wtransport::endpoint::SessionRequest,
    ) -> Result<Option<ClientConnectionResult>, wtransport::error::ConnectionError> {
        // Extract the client's first connection request from the request URL.
        //
        // SECURITY NOTE: Connection requests are sent *unencrypted*, which matches how they are
        // sent when using UDP sockets.
        // TODO: Consider authenticating UDP client addresses in connect tokens, and sending WebTransport
        // connection requests after sessions are established.
        let packet = extract_client_connection_req(session_request.path())?;

        // Assign an identifier to this client.
        let client_idx = client_iterator.fetch_add(1, Ordering::Relaxed);

        // Send connection request packet to netcode for evaluation.
        let (result_sender, mut result_receiver) = mpsc::channel::<ConnectionRequestResult>(1usize);
        let Ok(_) = connection_req_sender.try_send(ConnectionRequest {
            client_idx,
            packet,
            result_sender,
        }) else {
            return Ok(None);
        };

        // Wait for the result of evaluating the connection request.
        // - The connection must be validated before we accept the session to avoid resources being
        //   consumed by fake clients.
        let Some(ConnectionRequestResult::Success { client_id }) = result_receiver.recv().await else {
            return Ok(None);
        };

        // Finalize the connection.
        match session_request.accept().await {
            Ok(session) => Ok(Some(ClientConnectionResult::Success {
                client_idx,
                client_id,
                session,
            })),
            Err(err) => {
                // We must return failure here because `ConnectionRequestResult::Success` means the server
                // is tracking this connection. We need the server to clean up its pending client entry.
                debug!("Failed to handle connection: {err:?}");
                Ok(Some(ClientConnectionResult::Failure { client_idx }))
            }
        }
    }

    fn reading_thread(
        handle: &tokio::runtime::Handle,
        read_datagram: wtransport::Connection,
        sender: crossbeam::channel::Sender<Bytes>,
        mut abort_signal: mpsc::UnboundedReceiver<()>,
    ) -> tokio::task::JoinHandle<()> {
        handle.spawn(async move {
            // We must have a keep-alive timer here to ensure pending clients cannot occupy client slots after
            // their connect token has expired and they have been removed from the netcode server.
            // - Requiring incoming messages to reset the timer means pending clients will eventually cause netcode
            //   `ConnectionDenied` if they spam connection requests to maintain the keep-alive without becoming
            //   fully connected. Their connect token will time out and then new connection requests will be denied unless
            //   they get a fresh one. Obtaining a fresh connect token is considered an 'endorsement' from the service's
            //   architecture for the user's connection. Note that we kill pending clients if a new pending client usurps
            //   its client id, which ensures a specific user can't request a bunch of connect tokens in order to fill
            //   up client slots.
            let timeout = Duration::from_secs(5);
            let sleep = tokio::time::sleep(timeout);
            tokio::pin!(sleep);

            loop {
                tokio::select! {
                    // Prioritize the abort signal, deprioritize the sleep check.
                    biased;

                    _ = abort_signal.recv() => {
                        break;
                    },
                    Ok(datagram) = read_datagram.receive_datagram() => {
                        match sender.try_send(datagram.payload()) {
                            Ok(_) => {}
                            Err(err) => {
                                if let crossbeam::channel::TrySendError::Disconnected(_) = err {
                                    break;
                                }
                                trace!("The reading data could not be sent because the channel is currently full and sending \
                                    would require blocking.");
                            }
                        }
                    },
                    _ = &mut sleep => {
                        trace!("WT client socket reader timed out, disconnecting.");
                        break;
                    }
                    else => {
                        break;
                    },
                }

                sleep.as_mut().reset(tokio::time::Instant::now() + timeout);
            }
        })
    }
}

impl Drop for WebTransportServer {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::fmt::Debug for WebTransportServer {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

impl ServerSocket for WebTransportServer {
    fn is_encrypted(&self) -> bool {
        true
    }
    fn is_reliable(&self) -> bool {
        false
    }

    fn addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.addr)
    }

    fn is_closed(&mut self) -> bool {
        self.closed
    }

    fn close(&mut self) {
        self.close()
    }

    fn connection_denied(&mut self, addr: SocketAddr) {
        self.lost_clients.insert(client_idx_from_addr(addr));
    }

    fn connection_accepted(&mut self, client_id: u64, addr: SocketAddr) {
        let client_idx = client_idx_from_addr(addr);

        // If the client is not pending, then ignore this method call as spurious.
        // - Ignoring 'connection accepted' for non-pending clients avoids a race condition between a newer
        //   pending client's initial connection request, and secondary connection requests from accepted
        //   connections.
        let Some(pending_client) = self.pending_clients.get_mut(&client_idx) else {
            return;
        };

        // Notify the pending connection of success.
        let _ = pending_client
            .result_sender
            .try_send(ConnectionRequestResult::Success { client_id });
        pending_client.client_id = Some(client_id);

        // Insert this connection to the client id slot.
        if let Some(prev_client_idx) = self.client_id_to_idx.insert(client_id, client_idx) {
            // Sanity check the prev entry was a different connection.
            if prev_client_idx != client_idx {
                // Disconnect the previous connection that was using this client id slot.
                self.lost_clients.insert(prev_client_idx);
            }
        }
    }

    fn disconnect(&mut self, addr: SocketAddr) {
        self.lost_clients.insert(client_idx_from_addr(addr));
    }

    fn preupdate(&mut self) {
        // Save new connections.
        while let Ok(connection) = self.connection_receiver.try_recv() {
            // Check if the connection was a success.
            let (client_idx, client_id, session) = match connection {
                ClientConnectionResult::Success {
                    client_idx,
                    client_id,
                    session,
                } => (client_idx, client_id, session),
                ClientConnectionResult::Failure { client_idx } => {
                    self.lost_clients.insert(client_idx);
                    continue;
                }
            };

            // Remove tracked pending client.
            // - If the pending client is not tracked then discard the connection. This can happen if `Self::disconnect`
            //   was called while the accepted connection was in transit (unlikely but possible). It can also happen if
            //   another client usurped this connection's client id slot and caused it to be removed.
            let Some(pending_client) = self.pending_clients.remove(&client_idx) else {
                continue;
            };

            // Sanity check that this connection is still tied to its client id.
            // - It should not be possible for this to be false, since when a client id slot is usurped the
            //   pending client entry will be removed.
            if self.client_id_to_idx.get(&client_id) != Some(&client_idx) {
                error!(
                    "internal error: client id slot {:?} is occupied by another session on session connect",
                    client_id
                );
                self.current_clients.fetch_sub(1, Ordering::Release);
                return;
            }

            // Set up datagram reading for the session.
            let (sender, receiver) = crossbeam::channel::bounded::<Bytes>(256);
            let (abort_sender, abort_receiver) = mpsc::unbounded_channel::<()>();
            let thread = Self::reading_thread(&self.handle, session.clone(), sender, abort_receiver);
            self.clients.insert(
                client_idx,
                WebTransportServerClient {
                    session,
                    reader_receiver: receiver,
                    abort_sender,
                    reader_thread: thread,
                    client_id,
                },
            );

            // Forward the buffered packet to the client.
            // - It is safe to ignore send results here because the client is not connected to renet2 yet, it
            //   is only pending in netcode. Normally on error renet2 would want to disconnect the client from RenetServer.
            match pending_client.buffered_response {
                Some(buffered) => {
                    let _ = self.send(client_idx_to_addr(client_idx), &buffered[..]);
                }
                None => {
                    error!(
                        "internal error: pending client {:?} with id {:?} was missing a connection response",
                        pending_client.client_idx, pending_client.client_id
                    );
                }
            }
        }

        // Prep for receiving.
        self.recv_index = 0;
    }

    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        // Try to get the next connection request from pending connections.
        while let Ok(ConnectionRequest {
            client_idx,
            packet,
            result_sender,
        }) = self.connection_req_receiver.try_recv()
        {
            if packet.len() > buffer.len() {
                debug!(
                    "Payload for {} is too large {}, rejecting connection request",
                    client_idx,
                    packet.len()
                );
                // Discard the new client if it has a bad connection request.
                let _ = result_sender.try_send(ConnectionRequestResult::Failure);
                continue;
            }

            // Add pending client entry for its client idx.
            self.pending_clients
                .insert(client_idx, PendingClient::new(client_idx, result_sender));
            self.current_clients.fetch_add(1, Ordering::Release);

            buffer[..packet.len()].copy_from_slice(&packet[..]);
            return Ok((packet.len(), client_idx_to_addr(client_idx)));
        }

        // Search for the next-available message from accepted connections.
        let start_index = self.recv_index;
        let end_index = self.client_iterator.load(Ordering::Relaxed);
        for (client_idx, client_data) in self.clients.range((Included(&start_index), Excluded(&end_index))) {
            // Try to get a message from this client.
            if let Ok(packet) = client_data.reader_receiver.try_recv() {
                if packet.len() > buffer.len() {
                    debug!("Payload for {} is too large {}, disconnecting client", client_idx, packet.len());
                    self.lost_clients.insert(*client_idx); //want to call .disconnect() but can't take mut access to self
                    continue;
                }
                buffer[..packet.len()].copy_from_slice(&packet[..]);
                return Ok((packet.len(), client_idx_to_addr(*client_idx)));
            };

            // Update so the next time `try_recv` is called this client will be ignored (since it just failed to recv).
            self.recv_index = client_idx + 1;
        }

        // End condition after all clients have been drained.
        Err(std::io::Error::from(ErrorKind::WouldBlock))
    }

    fn postupdate(&mut self) {
        // Detect terminated clients.
        for (client_idx, client_data) in self.clients.iter() {
            if client_data.reader_thread.is_finished() {
                self.lost_clients.insert(*client_idx);
            }
        }

        // Remove lost clients.
        for client_idx in self.lost_clients.drain() {
            // Remove the client.
            let removed_client_id = {
                if let Some(client_data) = self.clients.remove(&client_idx) {
                    let _ = client_data.abort_sender.send(());
                    client_data.client_id
                } else if let Some(pending_client) = self.pending_clients.remove(&client_idx) {
                    let _ = pending_client.result_sender.try_send(ConnectionRequestResult::Failure);
                    pending_client.client_id.unwrap_or(u64::MAX)
                } else {
                    continue;
                }
            };

            // Only remove from count if the client was removed from a map. `lost_clients` can receive the same client
            // multiple times if `Self::disconnect` was called and then the client's reader thread later shuts down.
            let prev = self.current_clients.fetch_sub(1, Ordering::Release);
            debug_assert_eq!(prev.wrapping_sub(1), self.clients.len() + self.pending_clients.len());

            // Remove [client id : client idx] entry if the entry's client idx matches the removed client.
            if self.client_id_to_idx.get(&removed_client_id) == Some(&client_idx) {
                self.client_id_to_idx.remove(&removed_client_id);
            }
        }

        // Note: Lost clients will time out in NetcodeServer and be disconnected in RenetServer that way.
    }

    fn send(&mut self, addr: SocketAddr, packet: &[u8]) -> Result<(), NetcodeTransportError> {
        let client_idx = client_idx_from_addr(addr);

        let Some(client_data) = self.clients.get(&client_idx) else {
            // Buffer packet if directed to a pending client.
            if let Some(pending_client) = self.pending_clients.get_mut(&client_idx) {
                pending_client.set_buffer(packet);
                return Ok(());
            }

            return Err(std::io::Error::from(ErrorKind::ConnectionAborted).into());
        };

        let data = Bytes::copy_from_slice(packet);
        if let Err(err) = client_data.session.send_datagram(data) {
            // See https://www.rfc-editor.org/rfc/rfc9114.html#errors
            match err {
                SendDatagramError::NotConnected => {
                    self.disconnect(addr);
                    return Err(std::io::Error::from(ErrorKind::ConnectionAborted).into());
                }
                SendDatagramError::UnsupportedByPeer | SendDatagramError::TooLarge => debug!("Stream error: {err}"),
            }
        }

        Ok(())
    }
}

fn extract_client_connection_req(path: &str) -> Result<Vec<u8>, wtransport::error::ConnectionError> {
    let Some((_, query)) = path.split_once('?') else {
        log::trace!("invalid uri query, dropping connection request...");
        return Err(wtransport::error::ConnectionError::LocallyClosed);
    };
    let Some(encoded) = query.split_once(HTTP_CONNECT_REQ).and_then(|(_, r)| r.strip_prefix("=")) else {
        log::trace!("invalid uri query (missing req), dropping connection request...");
        return Err(wtransport::error::ConnectionError::LocallyClosed);
    };
    let connection_req = urlencoding::decode_binary(encoded.as_bytes());

    Ok(connection_req.into())
}
