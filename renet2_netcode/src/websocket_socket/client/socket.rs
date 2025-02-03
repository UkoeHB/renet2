use std::{
    io::ErrorKind,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use js_sys::Uint8Array;
use log::{debug, error, warn};
use wasm_bindgen::{closure::Closure, JsCast};
use wasm_bindgen_futures::spawn_local;
use web_sys::{BinaryType, CloseEvent, ErrorEvent, MessageEvent, WebSocket};

use crate::{ClientSocket, NetcodeTransportError, HTTP_CONNECT_REQ};

/// Configuration for setting up a [`WebSocketClient`].
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WebSocketClientConfig {
    /// The server's WebSocket URL that receives connections.
    pub server_url: url::Url,
}

impl WebSocketClientConfig {
    /// Extracts the server address from the server URL if it contains a `SocketAddr`.
    pub fn server_address(&self) -> Result<SocketAddr, anyhow::Error> {
        let host = self
            .server_url
            .host()
            .ok_or_else(|| std::io::Error::other("WebSocketClientConfig url does not have a host"))?;
        let port = self.server_url.port().unwrap_or_default();
        match host {
            url::Host::Domain(_) => {
                Err(std::io::Error::other("WebSocketClientConfig url is a domain but a SocketAddr was expected").into())
            }
            url::Host::Ipv4(ipv4) => Ok((ipv4, port).into()),
            url::Host::Ipv6(ipv6) => Ok((ipv6, port).into()),
        }
    }
}

/// Implementation of [`ClientSocket`] for WebSocket clients.
#[derive(Debug)]
pub struct WebSocketClient {
    server_url: url::Url,
    server_address: SocketAddr,
    server_has_tls: bool,
    connect_req_sender: async_channel::Sender<Vec<u8>>,
    incoming_receiver: async_channel::Receiver<Vec<u8>>,
    close_sender: async_channel::Sender<()>,
    outgoing_sender: async_channel::Sender<Vec<u8>>,
    closed: Arc<AtomicBool>,
    is_disconnected: bool,
    sent_connection_request: bool,
}

impl WebSocketClient {
    /// Makes a new WebSocket client that will connect to a WebSocket server.
    ///
    /// Can fail if the url scheme is not `ws` or `wss`.
    pub fn new(config: WebSocketClientConfig) -> Result<Self, anyhow::Error> {
        let server_address = config.server_address().unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let mut server_url = config.server_url.clone();
        let server_has_tls = match server_url.scheme() {
            "wss" => true,
            "ws" => false,
            other => {
                return Err(std::io::Error::other(format!(
                    "failed setting up websocket client, server url has '{other}' scheme instead of \
                    'ws' or 'wss'"
                ))
                .into());
            }
        };

        let (close_sender, close_receiver) = async_channel::unbounded::<()>();
        let (incoming_sender, incoming_receiver) = async_channel::unbounded::<Vec<u8>>();
        let (connect_req_sender, connect_req_receiver) = async_channel::bounded::<Vec<u8>>(1);
        let (outgoing_sender, outgoing_receiver) = async_channel::unbounded::<Vec<u8>>();
        let closed = Arc::new(AtomicBool::new(false));

        let inner_close_sender = close_sender.clone();
        let inner_closed = closed.clone();
        spawn_local(async move {
            // Wait for the initial connection request packet.
            let Ok(connection_req) = connect_req_receiver.recv().await else {
                inner_closed.store(true, Ordering::Relaxed);
                return;
            };

            // Build URL with connection request.
            let connect_msg_ser = urlencoding::encode_binary(&connection_req);
            server_url.set_query(Some(format!("{}={}", HTTP_CONNECT_REQ, &connect_msg_ser).as_str()));

            let ws = match WebSocket::new(server_url.as_str()) {
                Ok(ws) => ws,
                Err(err) => {
                    warn!(
                        "failed initializing websocket client with server url \"{}\": {err:?}",
                        server_url.as_str()
                    );
                    inner_closed.store(true, Ordering::Relaxed);
                    return;
                }
            };
            ws.set_binary_type(BinaryType::Arraybuffer);

            // Prep to receive messages.
            let message_closed = inner_closed.clone();
            let message_close_sender = inner_close_sender.clone();
            let on_message_callback = Closure::<dyn FnMut(_)>::new(move |e: MessageEvent| {
                let msg = Uint8Array::new(&e.data()).to_vec();
                if incoming_sender.try_send(msg).is_err() {
                    message_closed.store(true, Ordering::Relaxed);
                    let _ = message_close_sender.try_send(());
                }
            });
            let close_closed = inner_closed.clone();
            let close_close_sender = inner_close_sender.clone();
            let on_close_callback = Closure::<dyn FnMut(_)>::new(move |_: CloseEvent| {
                close_closed.store(true, Ordering::Relaxed);
                let _ = close_close_sender.try_send(());
            });
            ws.set_onmessage(Some(on_message_callback.as_ref().unchecked_ref()));
            ws.set_onclose(Some(on_close_callback.as_ref().unchecked_ref()));
            on_message_callback.forget();
            on_close_callback.forget();

            // Wait for the request to be accepted.
            let (open_sx, open_rx) = futures_channel::oneshot::channel();
            let on_open_callback = {
                let mut open_sx = Some(open_sx);
                Closure::wrap(Box::new(move |_event| {
                    open_sx.take().map(|open_sx| open_sx.send(()));
                }) as Box<dyn FnMut(web_sys::Event)>)
            };

            let (err_sx, err_rx) = futures_channel::oneshot::channel();
            let on_error_callback = {
                let mut err_sx = Some(err_sx);
                Closure::wrap(Box::new(move |_error_event| {
                    err_sx.take().map(|err_sx| err_sx.send(()));
                }) as Box<dyn FnMut(ErrorEvent)>)
            };
            ws.set_onerror(Some(on_error_callback.as_ref().unchecked_ref()));
            ws.set_onopen(Some(on_open_callback.as_ref().unchecked_ref()));
            on_error_callback.forget();
            on_open_callback.forget();

            let result = futures_util::future::select(open_rx, err_rx).await;
            ws.set_onopen(None);
            ws.set_onerror(None);
            let ws = match result {
                futures_util::future::Either::Left((_, _)) => ws,
                futures_util::future::Either::Right((_, _)) => {
                    inner_closed.store(true, Ordering::Relaxed);
                    let _ = inner_close_sender.try_send(());
                    return;
                }
            };

            // Handle errors post-connection.
            let err_closed = inner_closed.clone();
            let err_close_sender = inner_close_sender.clone();
            let on_error_callback = Closure::<dyn FnMut(_)>::new(move |e: ErrorEvent| {
                warn!("WebSocket connection error {}", e.message());
                err_closed.store(true, Ordering::Relaxed);
                let _ = err_close_sender.try_send(());
            });
            ws.set_onerror(Some(on_error_callback.as_ref().unchecked_ref()));
            on_error_callback.forget();

            // Forward messages to the connection.
            let ws_clone = ws.clone();
            let send_closed = inner_closed.clone();
            spawn_local(async move {
                while let Ok(msg) = outgoing_receiver.recv().await {
                    if ws_clone.ready_state() != 1 {
                        warn!("Tried to send packet through closed websocket connection");
                        break;
                    }
                    if ws_clone.send_with_u8_array(&msg).is_err() {
                        let _ = ws_clone.close();
                        break;
                    }
                }
                send_closed.store(true, Ordering::Relaxed);
                let _ = inner_close_sender.try_send(());
            });

            // Listen for manual closes.
            spawn_local(async move {
                let _ = close_receiver.recv().await;
                let _ = ws.close();
                inner_closed.store(true, Ordering::Relaxed);
            });
        });

        Ok(Self {
            server_url: config.server_url,
            server_address,
            server_has_tls,
            connect_req_sender,
            incoming_receiver,
            close_sender,
            outgoing_sender,
            closed,
            is_disconnected: false,
            sent_connection_request: false,
        })
    }

    pub fn is_disconnected(&self) -> bool {
        self.is_disconnected || self.closed.load(Ordering::Relaxed)
    }

    pub fn server_url(&self) -> &url::Url {
        &self.server_url
    }

    /// Returns a dummy address if the server url doesn't contain a SocketAddr.
    pub fn server_address(&self) -> SocketAddr {
        self.server_address
    }

    pub fn disconnect(&mut self) {
        let _ = self.close_sender.send(());
        self.is_disconnected = true;
        self.closed.store(true, Ordering::Relaxed);
    }
}

impl Drop for WebSocketClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

impl ClientSocket for WebSocketClient {
    fn is_encrypted(&self) -> bool {
        self.server_has_tls
    }
    fn is_reliable(&self) -> bool {
        true
    }

    fn addr(&self) -> std::io::Result<SocketAddr> {
        // WebSocket clients don't have a meaningful address.
        Err(std::io::Error::from(ErrorKind::AddrNotAvailable))
    }

    fn is_closed(&mut self) -> bool {
        self.is_disconnected()
    }

    fn close(&mut self) {
        self.disconnect()
    }

    fn preupdate(&mut self) {
        // Check for disconnect.
        if !self.is_disconnected && self.closed.load(Ordering::Relaxed) {
            self.disconnect();
        }
    }

    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        if self.is_closed() {
            return Err(std::io::Error::from(ErrorKind::ConnectionAborted));
        }

        let Ok(packet) = self.incoming_receiver.try_recv() else {
            return Err(std::io::Error::from(ErrorKind::WouldBlock));
        };

        if packet.len() > buffer.len() {
            return Err(std::io::Error::from(ErrorKind::InvalidData));
        }

        buffer[..packet.len()].copy_from_slice(&packet[..]);

        Ok((packet.len(), self.server_address()))
    }

    fn postupdate(&mut self) {}

    // This method will panic if not called on the main thread, which is not a problem for WASM which is single-threaded.
    fn send(&mut self, addr: SocketAddr, packet: &[u8]) -> Result<(), NetcodeTransportError> {
        if self.is_closed() {
            return Err(std::io::Error::from(ErrorKind::ConnectionAborted).into());
        }
        if addr != self.server_address() {
            error!("tried sending packet to invalid WebSocket server {}", addr);
            self.close();
            return Err(std::io::Error::from(ErrorKind::AddrNotAvailable).into());
        }

        // If we are just connecting for the first time, then the first message to send must be a connection request.
        if !self.sent_connection_request {
            // Ignore the packet if it is not a connection request.
            let packet_type = renetcode2::Packet::packet_type_from_buffer(packet)?;
            if packet_type != renetcode2::PacketType::ConnectionRequest {
                debug!(
                    "ignoring {:?}, the first packet sent to a webSocket client must be a connection request",
                    packet_type
                );
                return Ok(());
            }

            // Send the connection request.
            let mut data = Vec::default();
            data.extend_from_slice(packet);
            let _ = self.connect_req_sender.try_send(data);
            self.sent_connection_request = true;

            return Ok(());
        }

        // Forward packet from the client to the remote server.
        if let Err(_) = self.outgoing_sender.try_send(packet.into()) {
            self.close();
            return Err(std::io::Error::from(ErrorKind::ConnectionAborted).into());
        }

        Ok(())
    }
}
