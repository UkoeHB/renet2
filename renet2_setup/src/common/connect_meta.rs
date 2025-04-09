use renet2_netcode::{ConnectToken, ServerCertHash};
use serde::{Deserialize, Serialize};

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};

use crate::{connect_token_to_bytes, ConnectionType, ServerConnectToken};

//-------------------------------------------------------------------------------------------------------------------

/// Metadata required to generate connect tokens for in-memory clients.
#[cfg(feature = "memory_transport")]
#[derive(Debug, Clone)]
pub struct ConnectMetaMemory {
    pub protocol_id: u64,
    pub expire_secs: u64,
    pub timeout_secs: i32,
    pub clients: Vec<renet2_netcode::MemorySocketClient>,
    pub socket_id: u8,
    pub auth_key: [u8; 32],
}

#[cfg(feature = "memory_transport")]
impl ConnectMetaMemory {
    /// Generates a new connect token for an in-memory client.
    ///
    /// Note that [`ConnectMetaMemory`] can contain sockets for multiple clients. We search available clients for
    /// the requested client id, and return `None` on failure.
    pub fn new_connect_token(&self, current_time: Duration, client_id: u64) -> Result<ServerConnectToken, String> {
        let token = ConnectToken::generate(
            current_time,
            self.protocol_id,
            self.expire_secs,
            client_id,
            self.timeout_secs,
            self.socket_id,
            vec![renet2_netcode::in_memory_server_addr()],
            None,
            &self.auth_key,
        )
        .map_err(|err| format!("failed generating connect token: {err:?}"))?;

        let token = connect_token_to_bytes(&token).map_err(|err| format!("failed writing connect token to bytes: {err:?}"))?;
        let client = self
            .clients
            .iter()
            .find(|c| c.id() == client_id)
            .cloned()
            .ok_or_else(|| format!("failed constructing connect token, requested in-memory client {client_id} is unknown"))?;

        Ok(ServerConnectToken::Memory { token, client })
    }
}

#[cfg(not(feature = "memory_transport"))]
#[derive(Debug, Clone)]
pub struct ConnectMetaMemory;

//-------------------------------------------------------------------------------------------------------------------

/// Metadata required to generate connect tokens for native-target clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectMetaNative {
    pub protocol_id: u64,
    pub expire_secs: u64,
    pub timeout_secs: i32,
    pub server_addresses: Vec<SocketAddr>,
    pub socket_id: u8,
    pub auth_key: [u8; 32],
}

impl ConnectMetaNative {
    pub fn dummy() -> Self {
        let mut auth_key = [0u8; 32];
        auth_key[0] = 1;

        Self {
            protocol_id: 0u64,
            expire_secs: 10u64,
            timeout_secs: 15,
            server_addresses: vec![SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080u16))],
            socket_id: 0,
            auth_key,
        }
    }

    /// Generates a new connect token for a native client.
    pub fn new_connect_token(&self, current_time: Duration, client_id: u64) -> Result<ServerConnectToken, String> {
        let token = ConnectToken::generate(
            current_time,
            self.protocol_id,
            self.expire_secs,
            client_id,
            self.timeout_secs,
            self.socket_id,
            self.server_addresses.clone(),
            None,
            &self.auth_key,
        )
        .map_err(|err| format!("failed generating connect token: {err:?}"))?;

        let token = connect_token_to_bytes(&token).map_err(|err| format!("failed writing connect token to bytes: {err:?}"))?;
        Ok(ServerConnectToken::Native { token })
    }
}

//-------------------------------------------------------------------------------------------------------------------

/// Metadata required to generate connect tokens for wasm-target webtransport clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectMetaWasmWt {
    pub protocol_id: u64,
    pub expire_secs: u64,
    pub timeout_secs: i32,
    pub server_addresses: Vec<SocketAddr>,
    pub socket_id: u8,
    pub auth_key: [u8; 32],
    /// If PKI certs are used instead of certificate hashes then this vec should be empty.
    pub cert_hashes: Vec<ServerCertHash>,
}

impl ConnectMetaWasmWt {
    /// Generates a new connect token for a wasm webtransport client.
    pub fn new_connect_token(&self, current_time: Duration, client_id: u64) -> Result<ServerConnectToken, String> {
        let token = ConnectToken::generate(
            current_time,
            self.protocol_id,
            self.expire_secs,
            client_id,
            self.timeout_secs,
            self.socket_id,
            self.server_addresses.clone(),
            None,
            &self.auth_key,
        )
        .map_err(|err| format!("failed generating connect token: {err:?}"))?;

        let token = connect_token_to_bytes(&token).map_err(|err| format!("failed writing connect token to bytes: {err:?}"))?;

        Ok(ServerConnectToken::WasmWt {
            token,
            cert_hashes: self.cert_hashes.clone(),
        })
    }
}

//-------------------------------------------------------------------------------------------------------------------

/// Metadata required to generate connect tokens for wasm-target websocket clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectMetaWasmWs {
    pub protocol_id: u64,
    pub expire_secs: u64,
    pub timeout_secs: i32,
    pub server_addresses: Vec<SocketAddr>,
    pub socket_id: u8,
    pub auth_key: [u8; 32],
    pub url: url::Url,
}

impl ConnectMetaWasmWs {
    /// Generates a new connect token for a wasm websocket client.
    pub fn new_connect_token(&self, current_time: Duration, client_id: u64) -> Result<ServerConnectToken, String> {
        let token = ConnectToken::generate(
            current_time,
            self.protocol_id,
            self.expire_secs,
            client_id,
            self.timeout_secs,
            self.socket_id,
            self.server_addresses.clone(),
            None,
            &self.auth_key,
        )
        .map_err(|err| format!("failed generating connect token: {err:?}"))?;

        let token = connect_token_to_bytes(&token).map_err(|err| format!("failed writing connect token to bytes: {err:?}"))?;

        Ok(ServerConnectToken::WasmWs {
            token,
            url: self.url.clone(),
        })
    }
}

//-------------------------------------------------------------------------------------------------------------------

/// Metadata required to generate connect tokens for renet2 clients.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ConnectMetas {
    #[serde(skip)]
    pub memory: Option<ConnectMetaMemory>,
    pub native: Option<ConnectMetaNative>,
    pub wasm_wt: Option<ConnectMetaWasmWt>,
    pub wasm_ws: Option<ConnectMetaWasmWs>,
}

impl ConnectMetas {
    pub fn new_connect_token(
        &self,
        current_time: Duration,
        client_id: u64,
        connection_type: ConnectionType,
    ) -> Result<ServerConnectToken, String> {
        match connection_type {
            ConnectionType::Memory | ConnectionType::Native => {
                let Some(meta) = &self.native else {
                    return Err(format!("no native connect meta for native client"));
                };
                meta.new_connect_token(current_time, client_id)
                    .map_err(|err| format!("failed constructing native connect token: {err:?}"))
            }
            ConnectionType::WasmWtPkiCerts => {
                // Clients that request webtransport can fall back to websockets.
                match (&self.wasm_wt, &self.wasm_ws) {
                    // If cert hashes are set then this client can't connect to it.
                    (Some(meta), _) if meta.cert_hashes.len() > 0 => {
                        meta.new_connect_token(current_time, client_id)
                            .map_err(|err| format!("failed constructing wasm wt connect token for wasm client: {err:?}"))
                    }
                    (_, Some(meta)) => {
                        meta.new_connect_token(current_time, client_id)
                            .map_err(|err| format!("failed constructing wasm ws connect token for wasm client: {err:?}"))
                    }
                    _ => Err(format!("no wasm webtransport connect meta for wasm client"))
                }
            }
            ConnectionType::WasmWtCertHashes => {
                // Clients that request webtransport can fall back to websockets.
                if let Some(meta) = &self.wasm_wt {
                    meta.new_connect_token(current_time, client_id)
                        .map_err(|err| format!("failed constructing wasm wt connect token for wasm client: {err:?}"))
                } else if let Some(meta) = &self.wasm_ws {
                    meta.new_connect_token(current_time, client_id)
                        .map_err(|err| format!("failed constructing wasm ws connect token for wasm client: {err:?}"))
                } else {
                    Err(format!("no wasm webtransport connect meta for wasm client"))
                }
            }
            ConnectionType::WasmWs => {
                let Some(meta) = &self.wasm_ws else {
                    return Err(format!("no wasm websocket connect meta for wasm client"));
                };
                meta.new_connect_token(current_time, client_id)
                    .map_err(|err| format!("failed constructing wasm ws connect token for wasm client: {err:?}"))
            }
        }
    }
}

//-------------------------------------------------------------------------------------------------------------------
