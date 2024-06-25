use renet2::{transport::{CongestionControl, NetcodeClientTransport, ServerCertHash, WebServerDestination, WebServerDestinationSerializable, WebTransportClient, WebTransportClientConfig}, ConnectionConfig, DefaultChannel, RenetClient};
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

        // Wait for renet2 server connection info.
        let (server_dest, server_cert_hash) = reqwest::get("http://127.0.0.1:4433/wasm")
            .await.unwrap()
            .json::<(WebServerDestinationSerializable, ServerCertHash)>()
            .await.unwrap();
        let server_dest: WebServerDestination = server_dest.try_into().unwrap();

        // Setup
        let connection_config = ConnectionConfig::default();
        let client = RenetClient::new(connection_config);
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let client_auth = ClientAuthentication::Unsecure {
            client_id: current_time.as_millis() as u64,
            protocol_id: 0,
            socket_id: 1,  //Webtransport socket id is 1 in this example
            server_addr: server_dest.clone().into(),
            user_data: None,
        };
        let socket_config = WebTransportClientConfig{
            server_dest: server_dest.into(),
            congestion_control: CongestionControl::default(),
            server_cert_hashes: Vec::from([server_cert_hash]),
        };
        let socket = WebTransportClient::new(socket_config);

        let transport: NetcodeClientTransport = NetcodeClientTransport::new(current_time, client_auth, socket).unwrap();

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
