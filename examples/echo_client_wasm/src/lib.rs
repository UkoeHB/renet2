use renet2::{ConnectionConfig, DefaultChannel, RenetClient};
use renet2_netcode::{
    webtransport_is_available_with_cert_hashes, ClientSocket, CongestionControl, NetcodeClientTransport, ServerCertHash, WebServerDestination, WebSocketClient, WebSocketClientConfig, WebTransportClient, WebTransportClientConfig
};
use renetcode2::ClientAuthentication;
use std::time::Duration;
use wasm_bindgen::{prelude::wasm_bindgen, JsValue};
use wasm_timer::{SystemTime, UNIX_EPOCH};

#[wasm_bindgen]
pub struct ChatApplication {
    renet_client: RenetClient,
    transport_client: NetcodeClientTransport,
    time_last_update: Duration,
    messages: Vec<String>,
}

#[wasm_bindgen]
impl ChatApplication {
    pub async fn new() -> Result<ChatApplication, JsValue> {
        console_error_panic_hook::set_once();
        tracing_wasm::set_as_global_default();
        let _ = console_log::init_with_level(log::Level::Debug); // tracing_wasm doesn't unify with log crate

        // Wait for renet2 server connection info.
        tracing::info!("getting server info");
        let (wt_server_dest, wt_server_cert_hash, ws_server_url) = reqwest::get("http://127.0.0.1:4433/wasm")
            .await
            .unwrap()
            .json::<(WebServerDestination, ServerCertHash, url::Url)>()
            .await
            .unwrap();

        // Setup
        tracing::info!("setting up client");

        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let connection_config = ConnectionConfig::test();
        let (client, transport) = match webtransport_is_available_with_cert_hashes() {
            true => {
                tracing::info!("setting up webtransport client (server = {:?})", wt_server_dest);
                let client_auth = ClientAuthentication::Unsecure {
                    client_id: current_time.as_millis() as u64,
                    protocol_id: 0,
                    socket_id: 1, //WebTransport socket id is 1 in this example
                    server_addr: wt_server_dest.clone().into(),
                    user_data: None,
                };
                let socket_config = WebTransportClientConfig {
                    server_dest: wt_server_dest.into(),
                    congestion_control: CongestionControl::default(),
                    server_cert_hashes: Vec::from([wt_server_cert_hash]),
                };
                let socket = WebTransportClient::new(socket_config);

                let client = RenetClient::new(connection_config, socket.is_reliable());
                let transport = NetcodeClientTransport::new(current_time, client_auth, socket).unwrap();

                (client, transport)
            }
            false => {
                tracing::warn!("webtransport with cert hashes is not supported on this platform, falling back \
                    to websockets");
                tracing::info!("setting up websocket client (server = {:?})", ws_server_url.as_str());
                let socket_config = WebSocketClientConfig {
                    server_url: ws_server_url,
                };

                let socket = WebSocketClient::new(socket_config).unwrap();
                let client = RenetClient::new(connection_config, socket.is_reliable());

                let client_auth = ClientAuthentication::Unsecure {
                    client_id: current_time.as_millis() as u64,
                    protocol_id: 0,
                    socket_id: 2, //WebSocket socket id is 2 in this example
                    server_addr: socket.server_address(),
                    user_data: None,
                };
                let transport = NetcodeClientTransport::new(current_time, client_auth, socket).unwrap();

                (client, transport)
            }
        };

        Ok(Self {
            renet_client: client,
            transport_client: transport,
            time_last_update: current_time,
            messages: Vec::with_capacity(20),
        })
    }

    pub fn update(&mut self) {
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let delta = current_time.saturating_sub(self.time_last_update);
        self.time_last_update = current_time;
        let _ = self.transport_client.update(delta, &mut self.renet_client);

        if let Some(message) = self.renet_client.receive_message(DefaultChannel::Unreliable) {
            let message = String::from_utf8(message.into()).unwrap();
            self.messages.push(message);
        };
    }

    pub fn is_disconnected(&self) -> bool {
        if self.transport_client.is_disconnected() {
            panic!("{:?}", self.transport_client.disconnect_reason());
        }
        false
    }

    pub fn send_packets(&mut self) {
        let _ = self.transport_client.send_packets(&mut self.renet_client);
    }

    pub fn send_message(&mut self, message: &str) {
        self.renet_client
            .send_message(DefaultChannel::Unreliable, message.as_bytes().to_vec());
    }

    pub fn disconnect(mut self) {
        self.transport_client.disconnect();
    }

    pub fn get_messages(&self) -> String {
        self.messages.join("\n")
    }

    pub fn clear_messages(&mut self) {
        self.messages.clear();
    }
}
