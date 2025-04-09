use std::ops::Bound::{Excluded, Included};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::ErrorKind,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{SinkExt, StreamExt, TryStreamExt};
use http::Uri;
use tokio::{io::AsyncWriteExt, task::AbortHandle};
use tungstenite::handshake::server::{Request, Response};

use anyhow::Error;
use bytes::Bytes;
use tokio::sync::mpsc;

use crate::{client_idx_from_addr, client_idx_to_addr, NetcodeTransportError, ServerSocket, HTTP_CONNECT_REQ};

/// Acceptor config for WebSocket connections.
///
/// Note that if TLS is not used, then renetcode2's built-in encryption will be used instead. This
/// means TLS support is not security-critical (and may even be less efficient).
#[derive(Clone)]
pub enum WebSocketAcceptor {
    /// No TLS in the local server.
    Plain {
        /// Indicates if there is a TLS proxy outside the local server.
        has_tls_proxy: bool,
    },

    #[cfg(feature = "ws-native-tls")]
    NativeTls(tokio_native_tls::TlsAcceptor),

    #[cfg(feature = "ws-rustls")]
    Rustls(tokio_rustls::TlsAcceptor),
}

/// Configuration for setting up a [`WebSocketServer`].
pub struct WebSocketServerConfig {
    /// Connection acceptor for this server.
    pub acceptor: WebSocketAcceptor,
    /// Socket address to listen on.
    ///
    /// It is recommended to use a pre-defined IP and a wildcard port.
    ///
    /// Using a wildcard port will reduce your chance of competing with other sockets on your machine (e.g. other
    /// WebTransport servers running different game instances).
    pub listen: SocketAddr,
    /// Maximum number of active clients allowed.
    pub max_clients: usize,
}

impl WebSocketServerConfig {
    /// Makes a config without TLS or a TLS proxy.
    pub fn new(listen: SocketAddr, max_clients: usize) -> Self {
        Self {
            acceptor: WebSocketAcceptor::Plain { has_tls_proxy: false },
            listen,
            max_clients,
        }
    }
}

struct WebSocketServerClient {
    client_id: u64,
    client_idx: u64,
    incoming_receiver: crossbeam::channel::Receiver<Bytes>,
    outgoing_sender: mpsc::Sender<Bytes>,
    reader_handle: tokio::task::JoinHandle<()>,
    writer_handle: tokio::task::JoinHandle<()>,
    reader_aborter: mpsc::UnboundedSender<()>,
}

impl WebSocketServerClient {
    fn new<S>(socket: S, client_id: u64, client_idx: u64) -> Self
    where
        S: SinkExt<tungstenite::Message, Error = tungstenite::error::Error>
            + Unpin
            + StreamExt<Item = Result<tungstenite::Message, tungstenite::error::Error>>
            + Unpin
            + Send
            + 'static,
    {
        let (sink, stream) = socket.sink_err_into().err_into().split();

        // Setup reader.
        let (sender, incoming_receiver) = crossbeam::channel::bounded::<Bytes>(256);
        let (reader_aborter, abort_receiver) = mpsc::unbounded_channel::<()>();
        let reader_handle = tokio::spawn(async move { WebSocketServer::reading_thread(stream, sender, abort_receiver).await });

        // Setup writer.
        // - Writer must be in a thread because sending is async.
        let (outgoing_sender, receiver) = mpsc::channel::<Bytes>(256);
        let writer_handle = tokio::spawn(async move { WebSocketServer::writing_thread(sink, receiver, client_idx).await });

        Self {
            client_id,
            client_idx,
            incoming_receiver,
            outgoing_sender,
            reader_handle,
            writer_handle,
            reader_aborter,
        }
    }
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

/// Implementation of [`ServerSocket`] for WebSocket servers.
///
/// The server handles connections internally with `tokio`.
///
/// If the server is *not* connected with TLS, then connections will be encrypted using the
/// built-in `netcode` encryption implemented in `renetcode2` that is used for UDP connections.
/// This means TLS is likely not that useful for websocket connections.
///
/// See [`Self::url`] for details about how clients can connect to a websocket server.
pub struct WebSocketServer {
    addr: SocketAddr,
    has_tls: bool,

    connection_abort_handle: AbortHandle,

    connection_req_receiver: crossbeam::channel::Receiver<ConnectionRequest>,
    connection_receiver: crossbeam::channel::Receiver<WebSocketServerClient>,

    client_iterator: Arc<AtomicU64>,
    pending_clients: HashMap<u64, PendingClient>,
    clients: BTreeMap<u64, WebSocketServerClient>,
    /// Maps netcode client ids to internal client indices.
    client_id_to_idx: HashMap<u64, u64>,
    lost_clients: HashSet<u64>,

    closed: bool,
    current_clients: Arc<AtomicUsize>,
    recv_index: u64,
}

impl WebSocketServer {
    /// Makes a new server.
    ///
    /// ## Errors
    /// - Errors if unable to bind to `addr`, which can happen if your
    ///   machine is using all ports on a pre-defined IP address.
    pub fn new(config: WebSocketServerConfig, handle: tokio::runtime::Handle) -> Result<Self, Error> {
        let max_clients = config.max_clients;
        let has_tls = !matches!(config.acceptor, WebSocketAcceptor::Plain { has_tls_proxy: false });

        let socket = handle.block_on(async { tokio::net::TcpListener::bind(config.listen).await })?;
        let addr = socket.local_addr()?;

        // Channels
        let (connection_sender, connection_receiver) = crossbeam::channel::bounded::<WebSocketServerClient>(max_clients);
        let (connection_req_sender, connection_req_receiver) = crossbeam::channel::bounded::<ConnectionRequest>(max_clients);

        let client_iterator = Arc::new(AtomicU64::new(0));
        let current_clients = Arc::new(AtomicUsize::new(0));

        // Accept thread
        let inner_client_iterator = client_iterator.clone();
        let inner_current_clients = current_clients.clone();
        let connection_abort_handle = handle
            .spawn(Self::accept_connections(
                socket,
                config.acceptor,
                connection_sender.clone(),
                connection_req_sender.clone(),
                inner_client_iterator,
                inner_current_clients,
                max_clients,
            ))
            .abort_handle();
        Ok(Self {
            addr,
            has_tls,
            connection_abort_handle,
            connection_req_receiver,
            connection_receiver,
            client_iterator,
            pending_clients: HashMap::new(),
            clients: BTreeMap::new(),
            client_id_to_idx: HashMap::new(),
            lost_clients: HashSet::new(),
            closed: false,
            current_clients,
            recv_index: 0,
        })
    }

    /// Gets the server's local URL.
    ///
    /// The URL will have the format `{ws|wss}://[ip:port]`.
    ///
    /// The local URL likely differs from the public URL of the server due to NAT. The client should
    /// connect to the public URL, either by swapping the IP of this method's url for the public IP of your server
    /// (and possibly the port if doing port translation), or by replacing it with a domain name. If using
    /// a domain name then `ServerSocketConfig::server_addresses` and `ConnectToken` must be given the
    /// dummy `SocketAddr` `0.0.0.0:0`.
    pub fn url(&self) -> url::Url {
        make_websocket_url(self.has_tls, self.addr).unwrap()
    }

    /// Disconnects the server.
    pub fn close(&mut self) {
        self.connection_abort_handle.abort();
        self.closed = true;
    }

    async fn accept_connections(
        socket: tokio::net::TcpListener,
        acceptor: WebSocketAcceptor,
        connection_sender: crossbeam::channel::Sender<WebSocketServerClient>,
        connection_req_sender: crossbeam::channel::Sender<ConnectionRequest>,
        client_iterator: Arc<AtomicU64>,
        current_clients: Arc<AtomicUsize>,
        max_clients: usize,
    ) {
        while let Ok((mut stream, _)) = socket.accept().await {
            let acceptor = acceptor.clone();
            let connection_sender = connection_sender.clone();
            let connection_req_sender = connection_req_sender.clone();
            let current_clients = current_clients.clone();
            let client_iterator = client_iterator.clone();

            tokio::spawn(async move {
                let is_full = {
                    let current_clients = current_clients.load(Ordering::Relaxed);
                    // We allow 25% extra clients in case clients want to override their old sessions.
                    (current_clients * 4) >= (max_clients * 5)
                };
                if is_full {
                    stream.shutdown().await.ok();
                    log::debug!("Server is full, rejecting connection");
                    return;
                }

                match Self::handle_connection(acceptor, client_iterator, connection_req_sender, stream).await {
                    Ok(result) => {
                        if let Some(result) = result {
                            if let Err(err) = connection_sender.try_send(result) {
                                log::debug!("Failed to send connection result: {:?}", err);
                            }
                        }
                    }
                    Err(err) => {
                        log::debug!("Failed to handle connection: {:?}", err);
                    }
                }
            });
        }
    }

    async fn handle_connection(
        acceptor: WebSocketAcceptor,
        client_iterator: Arc<AtomicU64>,
        connection_req_sender: crossbeam::channel::Sender<ConnectionRequest>,
        conn: tokio::net::TcpStream,
    ) -> Result<Option<WebSocketServerClient>, Error> {
        let (uri_sender, mut uri_receiver) = mpsc::channel::<Uri>(1);
        // TODO: this is a multistep process that continues after receiving a Request. We would rather
        // pause to validate the URI before continuing, but tungstenite does not support that workflow.
        // Might need to use axum instead.
        let callback = move |req: &Request, res: Response| {
            let uri = req.uri().clone();
            uri_sender.try_send(uri).ok();
            Ok(res)
        };
        let make_server_client: Box<dyn FnOnce(u64, u64) -> WebSocketServerClient + Send + Sync> = match acceptor {
            WebSocketAcceptor::Plain { has_tls_proxy: _ } => {
                let socket = tokio_tungstenite::accept_hdr_async(conn, callback).await?;
                Box::new(move |client_id, client_idx| WebSocketServerClient::new(socket, client_id, client_idx))
            }
            #[cfg(feature = "ws-native-tls")]
            WebSocketAcceptor::NativeTls(acceptor) => {
                let tls_stream = acceptor.accept(conn).await?;
                let socket = tokio_tungstenite::accept_hdr_async(tls_stream, callback).await?;
                Box::new(move |client_id, client_idx| WebSocketServerClient::new(socket, client_id, client_idx))
            }
            #[cfg(feature = "ws-rustls")]
            WebSocketAcceptor::Rustls(acceptor) => {
                let tls_stream = acceptor.accept(conn).await?;
                let socket = tokio_tungstenite::accept_hdr_async(tls_stream, callback).await?;
                Box::new(move |client_id, client_idx| WebSocketServerClient::new(socket, client_id, client_idx))
            }
        };

        let Ok(uri) = uri_receiver.try_recv() else {
            return Ok(None);
        };

        // Extract the client's first connection request from the request URL.
        //
        // SECURITY NOTE: Connection requests are sent *unencrypted*, which matches how they are
        // sent when using UDP sockets.
        // TODO: Consider authenticating UDP client addresses in connect tokens, and sending
        // connection requests after sessions are established.
        let packet = extract_client_connection_req(&uri)?;

        // Assign an identifier to this client.
        let client_idx = client_iterator.fetch_add(1, Ordering::Relaxed);

        // Send connection request packet to netcode for evaluation.
        let (result_sender, mut result_receiver) = mpsc::channel::<ConnectionRequestResult>(1);
        if connection_req_sender
            .try_send(ConnectionRequest {
                client_idx,
                packet,
                result_sender,
            })
            .is_err()
        {
            return Ok(None);
        }

        // Wait for the result of evaluating the connection request.
        // - The connection must be validated before we accept the session to avoid resources being
        //   consumed by fake clients.
        let Some(ConnectionRequestResult::Success { client_id }) = result_receiver.recv().await else {
            return Ok(None);
        };

        // Finalize the connection.
        let server_client = (make_server_client)(client_id, client_idx);

        Ok(Some(server_client))
    }

    /// Receives packets from a client.
    async fn reading_thread<R: StreamExt<Item = Result<tungstenite::Message, tungstenite::error::Error>> + Unpin + Send + 'static>(
        mut ws_reader: R,
        sender: crossbeam::channel::Sender<Bytes>,
        mut abort_receiver: mpsc::UnboundedReceiver<()>,
    ) {
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

                _ = abort_receiver.recv() => {
                    break;
                },
                Some(result) = ws_reader.next() => match result {
                    Ok(msg) => {
                        let data = match msg {
                            tungstenite::Message::Binary(data) => data,
                            _ => {
                                log::trace!("WS client socket reader received a non-binary message, ignoring.");
                                continue;
                            },
                        };
                        match sender.try_send(Bytes::copy_from_slice(&data[..])) {
                            Ok(_) => {},
                            Err(err) => {
                                if let crossbeam::channel::TrySendError::Disconnected(_) = err {
                                    break;
                                }
                                log::trace!("The reading data could not be sent because the channel is currently full and sending \
                                    would require blocking.");
                            }
                        }
                    },
                    Err(err) => {
                        log::trace!("WS client socket reader encountered an error: {:?}", err);
                        break;
                    }
                },
                _ = &mut sleep => {
                    log::trace!("WS client socket reader timed out, disconnecting.");
                    break;
                }
                else => {
                    break;
                },
            }

            sleep.as_mut().reset(tokio::time::Instant::now() + timeout);
        }
    }

    /// Sends packets to a client.
    async fn writing_thread<S: SinkExt<tungstenite::Message, Error = tungstenite::error::Error> + Unpin + Send + 'static>(
        mut ws_writer: S,
        mut receiver: mpsc::Receiver<Bytes>,
        client_idx: u64,
    ) {
        while let Some(bytes) = receiver.recv().await {
            let msg = tungstenite::Message::Binary(bytes);
            // TODO: this isn't optimal because it flushes after every send instead of batching
            if let Err(err) = ws_writer.send(msg).await {
                log::trace!("Failed to send message to client {}: {:?}", client_idx, err);
                return;
            }
        }
    }
}

impl Drop for WebSocketServer {
    fn drop(&mut self) {
        if !self.closed {
            self.close();
        }
    }
}

impl std::fmt::Debug for WebSocketServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketServer")
            .field("addr", &self.addr)
            .field("closed", &self.closed)
            .finish()
    }
}

impl ServerSocket for WebSocketServer {
    fn is_encrypted(&self) -> bool {
        self.has_tls
    }
    fn is_reliable(&self) -> bool {
        true
    }

    fn addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.addr)
    }

    fn is_closed(&mut self) -> bool {
        self.closed
    }

    fn close(&mut self) {
        self.close();
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
        let client_idx = client_idx_from_addr(addr);
        self.lost_clients.insert(client_idx);
    }

    fn preupdate(&mut self) {
        while let Ok(server_client) = self.connection_receiver.try_recv() {
            let client_id = server_client.client_id;
            let client_idx = server_client.client_idx;

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
                log::error!(
                    "internal error: client id slot {:?} is occupied by another session on session connect",
                    client_id
                );
                self.current_clients.fetch_sub(1, Ordering::Release);
                return;
            }

            self.clients.insert(client_idx, server_client);

            // Forward the buffered packet to the client.
            // - It is safe to ignore send results here because the client is not connected to renet2 yet, it
            //   is only pending in netcode. Normally on error renet2 would want to disconnect the client from RenetServer.
            match pending_client.buffered_response {
                Some(buffered) => {
                    let _ = self.send(client_idx_to_addr(client_idx), &buffered[..]);
                }
                None => {
                    log::error!(
                        "internal error: pending client {:?} with id {:?} was missing a connection response",
                        pending_client.client_idx,
                        pending_client.client_id
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
                log::debug!(
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
            if let Ok(packet) = client_data.incoming_receiver.try_recv() {
                if packet.len() > buffer.len() {
                    log::debug!("Payload for {} is too large {}, disconnecting client", client_idx, packet.len());
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
            if client_data.reader_handle.is_finished() || client_data.writer_handle.is_finished() {
                self.lost_clients.insert(*client_idx);
            }
        }

        // Remove lost clients.
        for client_idx in self.lost_clients.drain() {
            // Remove the client.
            let removed_client_id = {
                if let Some(client_data) = self.clients.remove(&client_idx) {
                    let _ = client_data.reader_aborter.send(());
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

        // If the sender thread gets backed up because the client is unresponsive, then packets will be dropped.
        let data = Bytes::copy_from_slice(packet);
        match client_data.outgoing_sender.try_send(data) {
            Err(mpsc::error::TrySendError::Closed(_)) => return Err(std::io::Error::from(ErrorKind::ConnectionAborted).into()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                log::trace!("dropping packet for client {client_idx}; writer thread is backed up, client may be unresponsive");
            }
            Ok(()) => (),
        }

        Ok(())
    }
}

fn extract_client_connection_req(uri: &Uri) -> Result<Vec<u8>, Error> {
    let Some(query) = uri.query() else {
        log::trace!("invalid uri query, dropping connection request...");
        return Err(Error::msg("invalid uri query, dropping connection request..."));
    };
    let Some(encoded) = query.split_once(HTTP_CONNECT_REQ).and_then(|(_, r)| r.strip_prefix("=")) else {
        log::trace!("invalid uri query (missing req), dropping connection request...");
        return Err(Error::msg("invalid uri query (missing req), dropping connection request..."));
    };
    let connection_req = urlencoding::decode_binary(encoded.as_bytes());

    Ok(connection_req.into())
}

/// Makes a websocket url: `{ws, wss}://[ip:port]`.
fn make_websocket_url(with_tls: bool, address: SocketAddr) -> Result<url::Url, ()> {
    let mut url = url::Url::parse("https://example.net").map_err(|_| ())?;
    let scheme = match with_tls {
        true => "wss",
        false => "ws",
    };
    url.set_scheme(scheme)?;
    url.set_ip_host(address.ip())?;
    url.set_port(Some(address.port()))?;
    Ok(url)
}
