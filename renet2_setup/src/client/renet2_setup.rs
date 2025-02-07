#![allow(unused_imports)]

use renet2::{ConnectionConfig, RenetClient};
use renet2_netcode::{ClientAuthentication, ClientSocket, NetcodeClientTransport};

#[cfg(not(target_family = "wasm"))]
use std::net::{SocketAddr, UdpSocket};
use wasm_timer::SystemTime;

use crate::ClientConnectPack;

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet client with default transport using the provided authentication and client address.
#[cfg(all(not(target_family = "wasm"), feature = "native_transport"))]
fn setup_native_renet_client(
    authentication: ClientAuthentication,
    client_address: SocketAddr,
    connection_config: ConnectionConfig,
) -> Result<(RenetClient, NetcodeClientTransport), String> {
    // make client
    let udp_socket = UdpSocket::bind(client_address).map_err(|err| format!("failed binding {client_address:?}: {err:?}"))?;
    let client_socket =
        renet2_netcode::NativeSocket::new(udp_socket).map_err(|err| format!("failed constructing renet2 native socket: {err:?}"))?;
    let client = RenetClient::new(connection_config, client_socket.is_reliable());

    // make transport
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|err| format!("failed getting current time: {err:?}"))?;
    let transport = NetcodeClientTransport::new(current_time, authentication, client_socket)
        .map_err(|err| format!("failed constructing netcode client transport: {err:?}"))?;

    Ok((client, transport))
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet client with wasm webtransport transport using the provided authentication and client address.
#[cfg(all(target_family = "wasm", feature = "wt_client_transport"))]
fn setup_wasm_wt_renet_client(
    authentication: ClientAuthentication,
    config: renet2_netcode::WebTransportClientConfig,
    connection_config: ConnectionConfig,
) -> Result<(RenetClient, NetcodeClientTransport), String> {
    // make client
    let client_socket = renet2_netcode::WebTransportClient::new(config);
    let client = RenetClient::new(connection_config, client_socket.is_reliable());

    // make transport
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|err| format!("failed getting current time: {err:?}"))?;
    let transport = NetcodeClientTransport::new(current_time, authentication, client_socket)
        .map_err(|err| format!("failed constructing netcode client transport: {err:?}"))?;

    Ok((client, transport))
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet client with wasm websocket transport using the provided authentication and client address.
#[cfg(all(target_family = "wasm", feature = "ws_client_transport"))]
fn setup_wasm_ws_renet_client(
    authentication: ClientAuthentication,
    config: renet2_netcode::WebSocketClientConfig,
    connection_config: ConnectionConfig,
) -> Result<(RenetClient, NetcodeClientTransport), String> {
    // make client
    let client_socket =
        renet2_netcode::WebSocketClient::new(config).map_err(|err| format!("failed constructing websocket client: {err:?}"))?;
    let client = RenetClient::new(connection_config, client_socket.is_reliable());

    // make transport
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|err| format!("failed getting current time: {err:?}"))?;
    let transport = NetcodeClientTransport::new(current_time, authentication, client_socket)
        .map_err(|err| format!("failed constructing netcode client transport: {err:?}"))?;

    Ok((client, transport))
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet client with in-memory transport using the provided authentication and client socket.
#[cfg(feature = "memory_transport")]
fn setup_memory_renet_client(
    authentication: ClientAuthentication,
    client_socket: renet2_netcode::MemorySocketClient,
    connection_config: ConnectionConfig,
) -> Result<(RenetClient, NetcodeClientTransport), String> {
    // make client
    let client = RenetClient::new(connection_config, client_socket.is_reliable());

    // make transport
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|err| format!("failed getting current time: {err:?}"))?;
    let transport = NetcodeClientTransport::new(current_time, authentication, client_socket)
        .map_err(|err| format!("failed constructing netcode client transport: {err:?}"))?;

    Ok((client, transport))
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet2 client according to the given connection settings.
#[allow(unused_variables)]
pub fn setup_renet2_client(
    connection_config: ConnectionConfig,
    connect_pack: ClientConnectPack,
) -> Result<(RenetClient, NetcodeClientTransport), String> {
    log::info!("setting up renet2 client");

    match connect_pack {
        #[cfg(feature = "memory_transport")]
        ClientConnectPack::Memory(authentication, client) => setup_memory_renet_client(authentication, client, connection_config),
        ClientConnectPack::Native(_authentication, _client_address) => {
            #[cfg(target_family = "wasm")]
            {
                return Err(format!(
                    "failed setting up renet client with native connect pack; native connections \
                    not allowed in WASM environments"
                ));
            }

            #[cfg(all(not(target_family = "wasm"), not(feature = "native_transport")))]
            {
                return Err(format!(
                    "failed setting up renet client with native connect pack; native_transport feature is required"
                ));
            }

            #[cfg(all(not(target_family = "wasm"), feature = "native_transport"))]
            setup_native_renet_client(_authentication, _client_address, connection_config)
        }
        #[cfg(all(target_family = "wasm", feature = "wt_client_transport"))]
        ClientConnectPack::WasmWt(authentication, config) => setup_wasm_wt_renet_client(authentication, config, connection_config),
        #[cfg(all(target_family = "wasm", feature = "ws_client_transport"))]
        ClientConnectPack::WasmWs(authentication, config) => setup_wasm_ws_renet_client(authentication, config, connection_config),
    }
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet2 client and inserts the [`RenetClient`] and [`NetcodeClientTransport`] into `world` as
/// resources.
#[cfg(feature = "bevy")]
pub fn setup_renet2_client_in_bevy(
    world: &mut bevy_ecs::prelude::World,
    connection_config: ConnectionConfig,
    connect_pack: ClientConnectPack,
) -> Result<(), String> {
    // Drop the existing transport to free its address(es) in case we are re-using a client address.
    // - Note that this doesn't guarantee all addresses are freed, as some may not be freed until an async shutdown
    //   procedure is completed.
    world.remove_resource::<NetcodeClientTransport>();

    let (client, transport) = setup_renet2_client(connection_config, connect_pack)?;

    world.insert_resource(client);
    world.insert_resource(transport);

    Ok(())
}

//-------------------------------------------------------------------------------------------------------------------
