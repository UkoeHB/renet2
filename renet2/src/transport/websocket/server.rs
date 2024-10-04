use std::{collections::{BTreeMap, HashMap, HashSet}, io::ErrorKind, net::{IpAddr, Ipv6Addr, SocketAddr}, sync::{atomic::{AtomicU64, AtomicUsize, Ordering}, Arc, Mutex}, thread::JoinHandle, time::Duration};

use futures::{stream::{SplitSink, SplitStream}, SinkExt, StreamExt};
use tokio::{io::AsyncWriteExt, task::AbortHandle};
use http::Uri;
use tokio_tungstenite::WebSocketStream;
use tungstenite::{
    handshake::server::{Request, Response},
};

use anyhow::Error;
use bytes::Bytes;
use tokio::sync::mpsc;

use crate::transport::TransportSocket;

struct WebSocketServerClient {
    ws_writer: SplitSink<WebSocketStream<tokio::net::TcpStream>, tungstenite::Message>,
    reader_receiver: crossbeam::channel::Receiver<Bytes>,
    abort_sender: mpsc::UnboundedSender<()>,
    reader_thread: tokio::task::JoinHandle<()>,
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

#[derive(Debug)]
enum ClientConnectionResult {
    Success {
        client_idx: u64,
        client_id: u64,
        websocket: WebSocketStream<tokio::net::TcpStream>,
    },
    Failure {
        client_idx: u64,
    },
}

pub struct WebSocketServer {
    handle: tokio::runtime::Handle,

    addr: SocketAddr,

    connection_abort_handle: AbortHandle,

    connection_req_receiver: crossbeam::channel::Receiver<ConnectionRequest>,
    connection_receiver: crossbeam::channel::Receiver<ClientConnectionResult>,

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
    pub fn new(addr: SocketAddr, max_clients: usize, handle: tokio::runtime::Handle) -> Result<Self, Error> {
        
        let socket = handle.block_on(async {
            tokio::net::TcpListener::bind(addr).await
        })?;

        // Channels
        let (connection_sender, connection_receiver) = crossbeam::channel::bounded::<ClientConnectionResult>(max_clients);
        let (connection_req_sender, connection_req_receiver) = crossbeam::channel::bounded::<ConnectionRequest>(max_clients);

        let client_iterator = Arc::new(AtomicU64::new(0));
        let current_clients = Arc::new(AtomicUsize::new(0));

        // Accept thread
        let inner_client_iterator = client_iterator.clone();
        let inner_current_clients = current_clients.clone();
        let connection_abort_handle = handle.spawn(
            Self::accept_connection(
                socket,
                connection_sender.clone(),
                connection_req_sender.clone(),
                inner_client_iterator,
                inner_current_clients,
                max_clients,
            )
        ).abort_handle();
        Ok(Self {
            handle,
            addr,
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

    pub fn close(&mut self) {
        self.connection_abort_handle.abort();
        self.closed = true;
    }

    async fn accept_connection(
        socket: tokio::net::TcpListener,
        connection_sender: crossbeam::channel::Sender<ClientConnectionResult>,
        connection_req_sender: crossbeam::channel::Sender<ConnectionRequest>,
        client_iterator: Arc<AtomicU64>,
        current_clients: Arc<AtomicUsize>,
        max_clients: usize,
    ) {
        while let Ok((mut stream, _)) = socket.accept().await {

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
                
                match Self::handle_connection(client_iterator, connection_req_sender, stream).await {
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
        client_iterator: Arc<AtomicU64>,
        connection_req_sender: crossbeam::channel::Sender<ConnectionRequest>,
        conn: tokio::net::TcpStream,
    ) -> Result<Option<ClientConnectionResult>, Error> {

        let (uri_sender, mut uri_receiver) = mpsc::channel::<Uri>(1);
        let accept_res = {
            let accept_res = tokio_tungstenite::accept_hdr_async(conn, move |req: &Request, res: Response| {
                let uri = req.uri().clone();
                uri_sender.try_send(uri).ok();
                Ok(res)
            }).await;
            accept_res
        };
        match accept_res {
            Ok(ws_stream) => {
                let maybe_uri = uri_receiver.try_recv();
                if maybe_uri.is_err() {
                    return Ok(None);
                }
                let uri = maybe_uri.unwrap();

                let packet = extract_client_connection_req(&uri)?;
                let client_idx = client_iterator.fetch_add(1, Ordering::Relaxed);

                let (result_sender, mut result_receiver) = mpsc::channel::<ConnectionRequestResult>(1);
        
                if connection_req_sender.try_send(ConnectionRequest {
                    client_idx,
                    packet,
                    result_sender,
                }).is_err() {
                    return Ok(None);
                }
        
                let Some(ConnectionRequestResult::Success { client_id }) = result_receiver.recv().await else {
                    return Ok(None);
                };

                Ok(Some(ClientConnectionResult::Success {
                    client_idx,
                    client_id,
                    websocket: ws_stream,
                }))
            },
            Err(err) => {
                log::debug!("Failed to handle connection: {err:?}");
                Err(Error::from(err))
            }
        }
    }

    fn reading_thread(
        handle: tokio::runtime::Handle,
        mut ws_reader: SplitStream<WebSocketStream<tokio::net::TcpStream>>,
        sender: crossbeam::channel::Sender<Bytes>,
        mut abort_receiver: mpsc::UnboundedReceiver<()>,
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
                        log::trace!("WT client socket reader timed out, disconnecting.");
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

impl TransportSocket for WebSocketServer {
    fn is_encrypted(&self) -> bool {
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
        while let Ok(connection) = self.connection_receiver.try_recv() {
            // Check if the connection was a success.
            let (client_idx, client_id, websocket) = match connection {
                ClientConnectionResult::Success {
                    client_idx,
                    client_id,
                    websocket,
                } => (client_idx, client_id, websocket),
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
                log::error!(
                    "internal error: client id slot {:?} is occupied by another session on session connect",
                    client_id
                );
                self.current_clients.fetch_sub(1, Ordering::Release);
                return;
            }

            let (ws_writer, ws_reader) = websocket.split();
            let (sender, receiver) = crossbeam::channel::bounded::<Bytes>(256);
            let (abort_sender, abort_receiver) = mpsc::unbounded_channel::<()>();
            let thread = Self::reading_thread(self.handle.clone(), ws_reader, sender, abort_receiver);
            self.clients.insert(
                client_idx,
                WebSocketServerClient {
                    ws_writer,
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
                    log::error!(
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
                log::debug!(
                    "Payload for {} is too large {}, rejecting connection request",
                    client_idx,
                    packet.len()
                );
                // Discard the pending client if it has a bad connection request.
                let _ = result_sender.send(ConnectionRequestResult::Failure);
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
        for (client_idx, client_data) in self.clients.range((core::ops::Bound::Included(&start_index), core::ops::Bound::Excluded(&end_index))) {
            // Try to get a message from this client.
            if let Ok(packet) = client_data.reader_receiver.try_recv() {
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
                    let _ = pending_client.result_sender.send(ConnectionRequestResult::Failure);
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

    fn send(&mut self, addr: SocketAddr, packet: &[u8]) -> Result<(), crate::transport::NetcodeTransportError> {
        let client_idx = client_idx_from_addr(addr);

        let Some(client_data) = self.clients.get_mut(&client_idx) else {
            // Buffer packet if directed to a pending client.
            if let Some(pending_client) = self.pending_clients.get_mut(&client_idx) {
                pending_client.set_buffer(packet);
                return Ok(());
            }

            return Err(std::io::Error::from(ErrorKind::ConnectionAborted).into());
        };

        let data = Bytes::copy_from_slice(packet);
        let msg = tungstenite::Message::Binary(data.to_vec());
        if let Err(err) = self.handle.block_on(client_data.ws_writer.send(msg)) {
            log::trace!("Failed to send message to client {}: {:?}", client_idx, err);
            return Err(std::io::Error::from(ErrorKind::ConnectionAborted).into());
        }

        Ok(()) 
    }
}

const WS_CONNECT_REQ: &str = "creq";

fn extract_client_connection_req(uri: &Uri) -> Result<Vec<u8>, Error> {
    let Some(query) = uri.query() else {
        log::trace!("invalid uri query, dropping connection request...");
        return Err(Error::msg("invalid uri query, dropping connection request..."));
    };
    let mut query_elements_iterator = form_urlencoded::parse(query.as_bytes());
    let Some((key, connection_req)) = query_elements_iterator.next() else {
        log::trace!("invalid uri query (missing req), dropping connection request...");
        return Err(Error::msg("invalid uri query (missing req), dropping connection request..."));
    };
    if key != WS_CONNECT_REQ {
        log::trace!("invalid uri query (bad key), dropping connection request...");
    }
    let Ok(connection_req) = serde_json::de::from_str::<Vec<u8>>(&connection_req) else {
        log::trace!("invalid uri query (bad req), dropping connection request...");
        return Err(Error::msg("invalid uri query (bad req), dropping connection request..."));
    };

    Ok(connection_req)
}

fn client_idx_to_addr(idx: u64) -> SocketAddr {
    SocketAddr::new(
        IpAddr::V6(Ipv6Addr::new(
            idx as u16,
            (idx >> 16) as u16,
            (idx >> 32) as u16,
            (idx >> 48) as u16,
            0,
            0,
            0,
            0,
        )),
        0,
    )
}

fn client_idx_from_addr(addr: SocketAddr) -> u64 {
    let SocketAddr::V6(addr_v6) = addr else {
        panic!("V6 addresses are expected to represent client idxs")
    };
    let octets = addr_v6.ip().octets();

    let mut idx = 0u64;
    for i in (0..4).rev() {
        idx <<= 16;
        idx += ((octets[2 * i] as u64) << 8) + (octets[2 * i + 1] as u64);
    }

    idx
}
