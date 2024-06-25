use std::{
    io::ErrorKind,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use fragile::Fragile;
use js_sys::{Promise, Uint8Array};
use log::{debug, error, warn};
use renetcode2::NETCODE_MAX_PACKET_BYTES;
use send_wrapper::SendWrapper;
use wasm_bindgen::{prelude::Closure, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{ReadableStreamDefaultReader, WritableStreamDefaultWriter};

use crate::transport::{
    NetcodeTransportError, ServerCertHash, TransportSocket, WebServerDestination, WebServerDestinationSerializable, WT_CONNECT_REQ,
};

use super::bindings::{
    ReadableStreamDefaultReadResult, WebTransport, WebTransportCongestionControl, WebTransportError, WebTransportHash, WebTransportOptions,
};

/// Corresponds to the [`congestionControl`][congestion_control] WebTransport option.
///
/// Set to [`Self::LowLatency`] by default.
///
/// [congestion_control]: https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#congestionControl
#[derive(Debug, Default, Copy, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CongestionControl {
    Default,
    Throughput,
    #[default]
    LowLatency,
}

impl CongestionControl {
    fn to_wt(&self) -> WebTransportCongestionControl {
        match self {
            Self::Default => WebTransportCongestionControl::Default,
            Self::Throughput => WebTransportCongestionControl::Throughput,
            Self::LowLatency => WebTransportCongestionControl::LowLatency,
        }
    }
}

/// Configuration for setting up a [`WebTransportClient`].
///
/// *WARNING*: If your server destination is a `Url`, then `ServerSetupConfig` and `ClientAuthentication` must use
/// socket addresses derived from the url. See [`WebServerDestination`].
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WebTransportClientConfig {
    /// The server's address that receives connections.
    ///
    /// See [`WebServerDestination`].
    pub server_dest: WebServerDestinationSerializable,
    /// The [`CongestionControl`] that controls how channels are set up.
    ///
    /// Check [browser availability][browser_availability], as this is an experimental feature.
    ///
    /// [browser_availability]: (https://caniuse.com/mdn-api_webtransport_webtransport_options_congestioncontrol_parameter
    pub congestion_control: CongestionControl,
    /// The server's private certification hashes.
    ///
    /// If no hashes are provided, then the client will use the Web public key infrastructure (PKI) to validate
    /// the server's certificate.
    ///
    /// See the [docs][cert_hashes].
    ///
    /// Whether or not you use this may have a large impact on your architecture and dev-ops workflow. Be sure to
    /// check [browser availability][browser_availability], as this is an experimental feature.
    ///
    /// [cert_hashes]: https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#servercertificatehashes
    /// [browser_availability]: (https://caniuse.com/mdn-api_webtransport_webtransport_options_servercertificatehashes_parameter
    pub server_cert_hashes: Vec<ServerCertHash>,
}

impl WebTransportClientConfig {
    /// Makes a new config with default [`CongestionControl`] and no server cert hashes.
    ///
    /// The client will use the Web public key infrastructure (PKI) to validate the server's certificate.
    /// Use [`Self::new_with_certs`] if your server has a private authentication structure.
    pub fn new(server_dest: impl Into<WebServerDestination>) -> Self {
        Self {
            server_dest: server_dest.into().into(),
            congestion_control: CongestionControl::default(),
            server_cert_hashes: Vec::default(),
        }
    }

    /// Makes a new config with default [`CongestionControl`].
    pub fn new_with_certs(server_dest: impl Into<WebServerDestination>, server_cert_hashes: Vec<ServerCertHash>) -> Self {
        Self {
            server_dest: server_dest.into().into(),
            congestion_control: CongestionControl::default(),
            server_cert_hashes,
        }
    }
}

impl WebTransportClientConfig {
    /// Makes a [`WebTransportOptions`] from this config.
    /// - The [`allowPooling`][allow_pooling] field is set to false.
    /// - The [`requireUnreliable`][require_unreliable] field is set to true. The server should be using HTTP/3.
    ///
    /// [allow_pooling]: https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#allowPooling
    /// [require_unreliable]: https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#requireunreliable)
    pub fn wt_options(&self) -> WebTransportOptions {
        let mut options = WebTransportOptions::new();
        options.congestion_control(self.congestion_control.to_wt()).require_unreliable(true);

        if self.server_cert_hashes.len() > 0 {
            let cert_hashes = self
                .server_cert_hashes
                .iter()
                .map(|cert| {
                    let mut hash = WebTransportHash::new();
                    hash.algorithm("sha-256");
                    hash.value(&js_sys::Uint8Array::from(cert.hash.as_ref()));
                    wasm_bindgen::JsValue::from(hash)
                })
                .collect::<js_sys::Array>();

            options.server_certificate_hashes(&cert_hashes);
        }

        options
    }
}

/// Implementation of [`TransportSocket`] for WebTransport clients.
pub struct WebTransportClient {
    server_url: url::Url,
    server_address: SocketAddr,
    connect_req_sender: async_channel::Sender<Vec<u8>>,
    incoming_receiver: async_channel::Receiver<Vec<u8>>,
    close_sender: async_channel::Sender<()>,
    writer_receiver: async_channel::Receiver<Fragile<WritableStreamDefaultWriter>>,
    writer: Option<Fragile<WritableStreamDefaultWriter>>,
    closed: Arc<AtomicBool>,
    is_disconnected: bool,
    sent_connection_request: bool,
}

impl WebTransportClient {
    /// Makes a new WebTransport client that will connect to a WebTransport server.
    pub fn new(config: WebTransportClientConfig) -> Self {
        let options = config.wt_options();
        let server_dest: WebServerDestination = config.server_dest.try_into().expect("failed deserializing server destination");

        let (close_sender, close_receiver) = async_channel::unbounded::<()>();
        let (incoming_sender, incoming_receiver) = async_channel::unbounded::<Vec<u8>>();
        let (connect_req_sender, connect_req_receiver) = async_channel::bounded::<Vec<u8>>(1);
        let (writer_sender, writer_receiver) = async_channel::bounded::<Fragile<WritableStreamDefaultWriter>>(1);
        let closed = Arc::new(AtomicBool::new(false));

        let inner_server_dest = server_dest.clone();
        let inner_close_sender = close_sender.clone();
        let inner_closed = closed.clone();
        spawn_local(async move {
            // Wait for the initial connection request packet.
            let Ok(connection_req) = connect_req_receiver.recv().await else {
                inner_closed.store(true, Ordering::Relaxed);
                return;
            };

            // Build URL with connection request.
            let mut url: url::Url = inner_server_dest
                .clone()
                .try_into()
                .expect("could not convert server destination to url");
            let connect_msg_ser = serde_json::to_string(&connection_req).expect("could not serialize connect msg");
            url.query_pairs_mut().append_pair(WT_CONNECT_REQ, connect_msg_ser.as_str());

            // Set up WebTransport.
            let web_transport = match Self::init_web_transport(url.as_str(), options).await {
                Ok(web_transport) => web_transport,
                Err(err) => {
                    let _ = inner_close_sender.send(()).await;
                    warn!("failed setting up web transport client {:?}", err);
                    return;
                }
            };

            // Prep on-close callback.
            let close_callback_handle: Closure<dyn FnMut(JsValue)> = Self::get_close_callback(inner_close_sender);
            let _ = web_transport.closed().then(&close_callback_handle).catch(&close_callback_handle);

            // Prep writer.
            let writer: WritableStreamDefaultWriter = match web_transport.datagrams().writable().get_writer() {
                Ok(writer) => writer,
                Err(err) => {
                    web_transport.close();
                    warn!("failed setting up web transport client {:?}", err);
                    return;
                }
            };

            // Send writer to client if not yet closed.
            // - We need to be careful no race conditions exist where the writer won't be closed when the client has
            //   closed.
            if !inner_closed.load(Ordering::Relaxed) {
                let writer = Fragile::new(writer);
                let _ = writer_sender.try_send(writer);
            } else {
                handle_promise(writer.close());
                web_transport.close();
                return;
            }

            // Prep reader.
            let reader = web_transport.datagrams().readable().get_reader();
            let reader: ReadableStreamDefaultReader = JsValue::from(reader).into();
            let reader_closed = inner_closed.clone();
            Self::reader_task(reader, reader_closed, incoming_sender);

            // Wait for close.
            let _ = close_receiver.recv().await;

            inner_closed.store(true, Ordering::Relaxed);
            web_transport.close();
        });

        Self {
            server_url: server_dest.clone().try_into().expect("could not convert server destination to url"),
            server_address: server_dest.into(),
            connect_req_sender,
            incoming_receiver,
            close_sender,
            writer_receiver,
            writer: None,
            closed,
            is_disconnected: false,
            sent_connection_request: false,
        }
    }

    pub fn is_disconnected(&self) -> bool {
        self.is_disconnected || self.closed.load(Ordering::Relaxed)
    }

    pub fn server_url(&self) -> &url::Url {
        &self.server_url
    }

    pub fn server_address(&self) -> SocketAddr {
        self.server_address
    }

    pub fn disconnect(&mut self) {
        let _ = self.close_sender.send(());
        if let Ok(writer) = self.writer_receiver.try_recv() {
            // Collect the writer just in case it's stuck in its channel.
            self.writer = Some(writer);
        }
        if let Some(writer) = self.writer.as_ref().map(Fragile::get) {
            handle_promise(writer.close());
        }
        self.writer = None;
        self.is_disconnected = true;
        self.closed.store(true, Ordering::Relaxed);
    }

    async fn init_web_transport(url: &str, options: WebTransportOptions) -> Result<WebTransport, WebTransportError> {
        let web_transport = WebTransport::new_with_options(url, &options)?;
        // The Promise value is undefined and discarded, but once it completes the webtransport will be ready to use.
        JsFuture::from(web_transport.ready()).await?;
        Ok(web_transport)
    }

    /// Creates a closure to act on the close event
    fn get_close_callback(sender: async_channel::Sender<()>) -> Closure<dyn FnMut(JsValue)> {
        Closure::new(move |_| {
            let _ = sender.try_send(());
        })
    }

    /// Launches the reader task that receives packets from the server.
    fn reader_task(reader: ReadableStreamDefaultReader, reader_closed: Arc<AtomicBool>, incoming_sender: async_channel::Sender<Vec<u8>>) {
        spawn_local(async move {
            loop {
                if reader_closed.load(Ordering::Relaxed) {
                    break;
                }
                // This reader is expected to return an error if the webtransport is closed, which allows us to
                // wait here instead of checking for exit conditions manually (tokio:select does not work with JsFuture).
                let Ok(incoming) = JsFuture::from(reader.read()).await else { break };
                let result: ReadableStreamDefaultReadResult = incoming.into();
                if result.is_done() {
                    break;
                }
                let data: Uint8Array = result.value().into();
                if data.length() as usize > NETCODE_MAX_PACKET_BYTES {
                    error!("received packet that is too large from the webtransport server {}", data.length());
                    break;
                }
                // TODO: use a channel to pass vecs back to this loop so allocations can be reused.
                let Ok(()) = incoming_sender.try_send(data.to_vec()) else { break };
            }
        });
    }
}

impl Drop for WebTransportClient {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// Properly handles a promise.
///
/// A promise runs in the background, but it can have side effects when not handled correctly. See
/// https://github.com/typescript-eslint/typescript-eslint/blob/main/packages/eslint-plugin/docs/rules/no-floating-promises.md
fn handle_promise(promise: Promise) {
    type OptionalCallback = Option<SendWrapper<Closure<dyn FnMut(JsValue)>>>;
    static mut GET_NOTHING_CALLBACK_HANDLE: OptionalCallback = None;

    // SAFETY: WASM is single-threaded, so `SendWrapper` can be derefed safely.
    let nothing_callback_handle = unsafe {
        if GET_NOTHING_CALLBACK_HANDLE.is_none() {
            let cached_callback = Closure::new(|_| {});
            GET_NOTHING_CALLBACK_HANDLE = Some(SendWrapper::new(cached_callback));
        }

        GET_NOTHING_CALLBACK_HANDLE.as_deref().unwrap()
    };

    let _ = promise.catch(nothing_callback_handle);
}

impl std::fmt::Debug for WebTransportClient {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

impl TransportSocket for WebTransportClient {
    fn is_encrypted(&self) -> bool {
        true
    }

    fn addr(&self) -> std::io::Result<SocketAddr> {
        // WebTransport clients don't have a meaningful address.
        Err(std::io::Error::from(ErrorKind::AddrNotAvailable))
    }

    fn is_closed(&mut self) -> bool {
        self.is_disconnected()
    }

    fn close(&mut self) {
        self.disconnect()
    }

    fn connection_denied(&mut self, _: SocketAddr) {}
    fn connection_accepted(&mut self, _: u64, _: SocketAddr) {}
    fn disconnect(&mut self, _: SocketAddr) {}

    fn preupdate(&mut self) {
        // Check for disconnect.
        if !self.is_disconnected && self.closed.load(Ordering::Relaxed) {
            self.disconnect();
        }

        // Collect the writer after init.
        if self.writer.is_none() {
            if let Ok(writer) = self.writer_receiver.try_recv() {
                self.writer = Some(writer);
            }
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
            error!("tried sending packet to invalid WebTransport server {}", addr);
            return Err(std::io::Error::from(ErrorKind::AddrNotAvailable).into());
        }

        // If we are just connecting for the first time, then the first message to send must be a connection request.
        if !self.sent_connection_request {
            // Ignore the packet if it is not a connection request.
            let packet_type = renetcode2::Packet::packet_type_from_buffer(packet)?;
            if packet_type != renetcode2::PacketType::ConnectionRequest {
                debug!(
                    "ignoring {:?}, the first packet sent to a webtransport client must be a connection request",
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
        let Some(writer) = self.writer.as_ref().map(Fragile::get) else {
            // Ignore packet if the writer isn't available yet.
            return Ok(());
        };

        let net_packet = Uint8Array::new_with_length(packet.len() as u32);
        net_packet.copy_from(packet);
        handle_promise(writer.write_with_chunk(&net_packet.into()));

        Ok(())
    }
}
