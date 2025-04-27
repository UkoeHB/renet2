use crate::{client_address_from_server_address, connect_token_from_bytes, ServerConnectToken};

use renet2_netcode::ClientAuthentication;

use std::net::SocketAddr;

//-------------------------------------------------------------------------------------------------------------------

/// Information needed to connect a renet2 client to a renet2 server.
///
/// Connect packs should be considered single-use. If you need to reconnect, make a new connect pack with fresh
/// client authentication.
///
/// Implements `Resource` when the `bevy` feature is enabled.
#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::resource::Resource))]
pub enum ClientConnectPack {
    /// Connection information for native transports.
    ///
    /// Note: The client address should be tailored to the server address type (Ipv4/Ipv6).
    Native(ClientAuthentication, SocketAddr),
    /// Connection information for wasm webtransport transports.
    #[cfg(all(target_family = "wasm", feature = "wt_client_transport"))]
    WasmWt(ClientAuthentication, renet2_netcode::WebTransportClientConfig),
    /// Connection information for wasm websocket transports.
    #[cfg(all(target_family = "wasm", feature = "ws_client_transport"))]
    WasmWs(ClientAuthentication, renet2_netcode::WebSocketClientConfig),
    #[cfg(feature = "memory_transport")]
    Memory(ClientAuthentication, renet2_netcode::MemorySocketClient),
}

impl ClientConnectPack {
    /// Make a new connect pack from a server connect token.
    pub fn new(expected_protocol_id: u64, token: ServerConnectToken) -> Result<Self, String> {
        match token {
            ServerConnectToken::Native { token } => {
                // Extract renet2 ConnectToken.
                let connect_token =
                    connect_token_from_bytes(&token).map_err(|err| format!("failed deserializing connect token: {err:?}"))?;
                if connect_token.protocol_id != expected_protocol_id {
                    return Err(String::from("protocol id mismatch"));
                }

                // prepare client address based on server address
                let Some(server_addr) = connect_token.server_addresses[0] else {
                    return Err(String::from("server address is missing"));
                };
                let client_address = client_address_from_server_address(&server_addr);

                Ok(Self::Native(ClientAuthentication::Secure { connect_token }, client_address))
            }
            #[allow(unused_variables)]
            ServerConnectToken::WasmWt { token, cert_hashes } => {
                #[cfg(all(target_family = "wasm", feature = "wt_client_transport"))]
                {
                    // Extract renet2 ConnectToken.
                    let connect_token =
                        connect_token_from_bytes(&token).map_err(|err| format!("failed deserializing connect token: {err:?}"))?;
                    if connect_token.protocol_id != expected_protocol_id {
                        return Err(String::from("protocol id mismatch"));
                    }

                    // prepare client config based on server address
                    let Some(server_addr) = connect_token.server_addresses[0] else {
                        return Err(String::from("server address is missing"));
                    };
                    let config = renet2_netcode::WebTransportClientConfig::new_with_certs(server_addr, cert_hashes);

                    return Ok(Self::WasmWt(ClientAuthentication::Secure { connect_token }, config));
                }

                #[cfg(not(all(target_family = "wasm", feature = "wt_client_transport")))]
                return Err(format!(
                    "ServerConnectToken::WasmWt can only be converted to ClientConnectPack in WASM with \
                    wt_client_transport feature"
                ));
            }
            #[allow(unused_variables)]
            ServerConnectToken::WasmWs { token, url } => {
                #[cfg(all(target_family = "wasm", feature = "ws_client_transport"))]
                {
                    // Extract renet2 ConnectToken.
                    let connect_token =
                        connect_token_from_bytes(&token).map_err(|err| format!("failed deserializing connect token: {err:?}"))?;
                    if connect_token.protocol_id != expected_protocol_id {
                        return Err(String::from("protocol id mismatch"));
                    }

                    // prepare client config based on server url
                    if connect_token.server_addresses[0].is_none() {
                        return Err(String::from("server address is missing"));
                    };
                    let config = renet2_netcode::WebSocketClientConfig { server_url: url };

                    return Ok(Self::WasmWs(ClientAuthentication::Secure { connect_token }, config));
                }

                #[cfg(not(all(target_family = "wasm", feature = "ws_client_transport")))]
                return Err(format!(
                    "ServerConnectToken::WasmWs can only be converted to ClientConnectPack in WASM with \
                    ws_client_transport feature"
                ));
            }
            #[cfg(feature = "memory_transport")]
            ServerConnectToken::Memory { token, client } => {
                // Extract renet2 ConnectToken.
                let connect_token =
                    connect_token_from_bytes(&token).map_err(|err| format!("failed deserializing connect token: {err:?}"))?;
                if connect_token.protocol_id != expected_protocol_id {
                    return Err(String::from("protocol id mismatch"));
                }

                Ok(Self::Memory(ClientAuthentication::Secure { connect_token }, client))
            }
        }
    }
}

//-------------------------------------------------------------------------------------------------------------------
