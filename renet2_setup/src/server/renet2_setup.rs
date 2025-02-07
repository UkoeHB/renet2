use crate::common::{ConnectMetaNative, ConnectMetaWasmWs, ConnectMetaWasmWt, ConnectMetas, GameServerSetupConfig};
use renet2::{ConnectionConfig, RenetServer};
use renet2_netcode::{BoxedSocket, NetcodeServerTransport, ServerAuthentication, ServerSetupConfig};

use std::net::SocketAddr;
use wasm_timer::{SystemTime, UNIX_EPOCH};

use super::ClientCounts;

//-------------------------------------------------------------------------------------------------------------------

/// Makes a websocket url: `{ws, wss}://[{ip, domain}:port]/ws`.
#[cfg(feature = "ws_server_transport")]
fn make_websocket_url(with_tls: bool, ip: std::net::IpAddr, port: u16, maybe_domain: Option<String>) -> Result<url::Url, url::ParseError> {
    let mut url = url::Url::parse("https://example.net")?;
    let scheme = match with_tls {
        true => "wss",
        false => "ws",
    };
    url.set_scheme(scheme).map_err(|_| url::ParseError::EmptyHost)?;
    match maybe_domain {
        Some(domain) => url.set_host(Some(domain.as_str()))?,
        None => url.set_ip_host(ip).map_err(|_| url::ParseError::InvalidIpv4Address)?,
    }
    url.set_port(Some(port)).map_err(|_| url::ParseError::InvalidPort)?;
    url.set_path("/ws");
    Ok(url)
}

//-------------------------------------------------------------------------------------------------------------------

#[allow(unused_variables)]
fn add_memory_socket(
    config: &GameServerSetupConfig,
    memory_clients: Vec<u16>,
    socket_addresses: &mut Vec<Vec<SocketAddr>>,
    sockets: &mut Vec<BoxedSocket>,
    auth_key: &[u8; 32],
) -> Result<Option<crate::ConnectMetaMemory>, String> {
    if memory_clients.len() == 0 {
        return Ok(None);
    }

    #[cfg(not(feature = "memory_transport"))]
    {
        return Err(format!(
            "tried setting up renet2 server with in-memory clients, but memory_transport feature \
            is not enabled"
        ));
    }

    #[cfg(feature = "memory_transport")]
    {
        let (server_socket, client_sockets) = renet2_netcode::new_memory_sockets(memory_clients, true, true);
        let addrs = vec![renet2_netcode::in_memory_server_addr()];

        let meta = crate::ConnectMetaMemory {
            server_config: config.clone(),
            clients: client_sockets,
            socket_id: sockets.len() as u8, // DO THIS BEFORE PUSHING SOCKET
            auth_key: auth_key.clone(),
        };

        socket_addresses.push(addrs);
        sockets.push(BoxedSocket::new(server_socket));

        Ok(Some(meta))
    }
}

//-------------------------------------------------------------------------------------------------------------------

#[allow(unused_variables)]
fn add_native_socket(
    config: &GameServerSetupConfig,
    native_count: usize,
    socket_addresses: &mut Vec<Vec<SocketAddr>>,
    sockets: &mut Vec<BoxedSocket>,
    auth_key: &[u8; 32],
) -> Result<Option<ConnectMetaNative>, String> {
    if native_count == 0 {
        return Ok(None);
    }

    #[cfg(not(feature = "native_transport"))]
    {
        return Err(format!(
            "tried setting up renet2 server with native clients, but native_transport feature \
            is not enabled"
        ));
    }

    #[cfg(feature = "native_transport")]
    {
        use renet2_netcode::ServerSocket;
        let wildcard_addr = SocketAddr::new(config.server_ip, config.native_port);
        let server_socket = std::net::UdpSocket::bind(wildcard_addr)
            .map_err(|err| format!("failed binding renet2 server address {wildcard_addr:?}: {err:?}"))?;
        let socket =
            renet2_netcode::NativeSocket::new(server_socket).map_err(|err| format!("failed constructing renet2 native socket: {err:?}"))?;
        let local_addr = socket
            .addr()
            .map_err(|err| format!("failed getting local addr for renet2 native socket: {err:?}"))?;
        let addrs =
            if let Some(proxy) = config.proxy_ip { vec![SocketAddr::new(proxy.clone(), local_addr.port())] } else { vec![local_addr] };

        let meta = ConnectMetaNative {
            server_config: config.clone(),
            server_addresses: addrs.clone(),
            socket_id: sockets.len() as u8, // DO THIS BEFORE PUSHING SOCKET
            auth_key: auth_key.clone(),
        };

        log::info!("native renet2 socket; local addr = {}, public addr = {}", local_addr, addrs[0]);

        socket_addresses.push(addrs);
        sockets.push(BoxedSocket::new(socket));

        Ok(Some(meta))
    }
}

//-------------------------------------------------------------------------------------------------------------------

#[allow(unused_variables)]
fn add_wasm_wt_socket(
    config: &GameServerSetupConfig,
    count: usize,
    socket_addresses: &mut Vec<Vec<SocketAddr>>,
    sockets: &mut Vec<BoxedSocket>,
    auth_key: &[u8; 32],
) -> Result<Option<ConnectMetaWasmWt>, String> {
    if count == 0 {
        return Ok(None);
    }

    #[cfg(not(feature = "wt_server_transport"))]
    {
        return Err(format!(
            "tried setting up renet2 server with wasm webtransport clients, but \
            wt_server_transport feature is not enabled"
        ));
    }

    #[cfg(feature = "wt_server_transport")]
    {
        use enfync::AdoptOrDefault;
        use renet2_netcode::ServerSocket;
        let wildcard_addr = SocketAddr::new(config.server_ip, config.wasm_wt_port);
        let (wt_config, cert_hash) = renet2_netcode::WebTransportServerConfig::new_selfsigned(wildcard_addr, count)
            .map_err(|err| format!("failed constructing renet2 webtransport socket config: {err:?}"))?;
        let handle = enfync::builtin::native::TokioHandle::adopt_or_default(); //todo: don't depend on tokio...
        let socket = renet2_netcode::WebTransportServer::new(wt_config, handle.0)
            .map_err(|err| format!("failed constructing renet2 webtransport socket: {err:?}"))?;
        let local_addr = socket
            .addr()
            .map_err(|err| format!("failed getting local addr for renet2 webtransport socket: {err:?}"))?;
        let addrs =
            if let Some(proxy) = config.proxy_ip { vec![SocketAddr::new(proxy.clone(), local_addr.port())] } else { vec![local_addr] };

        let meta = ConnectMetaWasmWt {
            server_config: config.clone(),
            server_addresses: addrs.clone(),
            socket_id: sockets.len() as u8, // DO THIS BEFORE PUSHING SOCKET
            auth_key: auth_key.clone(),
            cert_hashes: vec![cert_hash],
        };

        log::info!(
            "wasm webtransport renet2 socket; local addr = {}, public addr = {}",
            local_addr,
            addrs[0]
        );

        socket_addresses.push(addrs);
        sockets.push(BoxedSocket::new(socket));

        Ok(Some(meta))
    }
}

//-------------------------------------------------------------------------------------------------------------------

#[allow(unused_variables)]
fn add_wasm_ws_socket(
    config: &GameServerSetupConfig,
    count: usize,
    socket_addresses: &mut Vec<Vec<SocketAddr>>,
    sockets: &mut Vec<BoxedSocket>,
    auth_key: &[u8; 32],
) -> Result<Option<ConnectMetaWasmWs>, String> {
    if count == 0 {
        return Ok(None);
    }

    #[cfg(not(feature = "ws_server_transport"))]
    {
        return Err(format!(
            "tried setting up renet2 server with wasm websocket clients, but ws_server_transport \
            feature is not enabled"
        ));
    }

    #[cfg(feature = "ws_server_transport")]
    {
        use enfync::AdoptOrDefault;
        use renet2_netcode::ServerSocket;
        let acceptor = config.get_ws_acceptor()?;
        let wildcard_addr = SocketAddr::new(config.server_ip, config.wasm_ws_port);
        let ws_config = renet2_netcode::WebSocketServerConfig {
            acceptor,
            listen: wildcard_addr,
            max_clients: count,
        };
        let handle = enfync::builtin::native::TokioHandle::adopt_or_default(); //todo: don't depend on tokio...
        let socket = renet2_netcode::WebSocketServer::new(ws_config, handle.0)
            .map_err(|err| format!("failed constructing renet2 websocket socket: {err:?}"))?;
        let local_addr = socket
            .addr()
            .map_err(|err| format!("failed getting local addr for renet2 native socket: {err:?}"))?;
        let addrs = if config.ws_domain.is_some() {
            // Dummy public address when using a domain name.
            vec![SocketAddr::from(([0, 0, 0, 0], 0))]
        } else if let Some(proxy) = config.proxy_ip {
            vec![SocketAddr::new(proxy.clone(), local_addr.port())]
        } else {
            vec![local_addr]
        };
        let url = make_websocket_url(socket.is_encrypted(), addrs[0].ip(), local_addr.port(), config.ws_domain.clone())
            .map_err(|err| format!("failed constructing renet2 websocket url: {err:?}"))?;

        log::info!("wasm websockets renet2 socket; local addr = {}, url = {}", local_addr, url);

        let meta = ConnectMetaWasmWs {
            server_config: config.clone(),
            server_addresses: addrs.clone(),
            socket_id: sockets.len() as u8, // DO THIS BEFORE PUSHING SOCKET
            auth_key: auth_key.clone(),
            url,
        };

        socket_addresses.push(addrs);
        sockets.push(BoxedSocket::new(socket));

        Ok(Some(meta))
    }
}

//-------------------------------------------------------------------------------------------------------------------

#[cfg(feature = "native_transport")]
fn _create_native_server(
    connection_config: ConnectionConfig,
    mut server_config: ServerSetupConfig,
) -> Result<(RenetServer, NetcodeServerTransport), String> {
    // make server
    let server = RenetServer::new(connection_config);

    // prepare udp socket
    // - finalizes the public address (wildcards should be resolved)
    let server_socket = std::net::UdpSocket::bind(server_config.socket_addresses[0][0]).map_err(|err| {
        format!(
            "failed binding renet2 server address {:?}: {err:?}",
            server_config.socket_addresses[0][0]
        )
    })?;
    let local_addr = server_socket
        .local_addr()
        .map_err(|err| format!("failed getting local addr for renet2 native socket: {err:?}"))?;
    server_config.socket_addresses = vec![vec![local_addr]];

    // make transport
    let server_transport = NetcodeServerTransport::new(
        server_config,
        renet2_netcode::NativeSocket::new(server_socket).map_err(|err| format!("failed constructing renet2 native socket: {err:?}"))?,
    )
    .map_err(|err| format!("failed constructing renet2 netcode server transport: {err:?}"))?;

    Ok((server, server_transport))
}

//-------------------------------------------------------------------------------------------------------------------

/// Set up a renet2 server with default transport using the provided [`ServerSetupConfig`].
#[cfg(all(feature = "bevy", feature = "native_transport"))]
pub fn setup_native_renet_server_in_bevy(
    server_world: &mut bevy_ecs::prelude::World,
    server_config: ServerSetupConfig,
    connection_config: ConnectionConfig,
) -> Result<SocketAddr, String> {
    log::info!("setting up renet2 server");

    // make server
    let (server, server_transport) = _create_native_server(connection_config, server_config)?;

    // add server to app
    let server_addr = server_transport.addresses()[0];
    server_world.insert_resource(server);
    server_world.insert_resource(server_transport);

    Ok(server_addr)
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet2 server with arbitrary combinations of memory/native/wasm transports.
pub fn setup_combo_renet2_server_with_key(
    config: GameServerSetupConfig,
    counts: ClientCounts,
    connection_config: ConnectionConfig,
    auth_key: &[u8; 32],
) -> Result<(RenetServer, NetcodeServerTransport, ConnectMetas), String> {
    log::info!("setting up renet2 server");

    let max_clients = counts.total();

    // add sockets
    let mut socket_addresses = Vec::default();
    let mut sockets = Vec::default();

    let memory_meta = add_memory_socket(&config, counts.memory_clients, &mut socket_addresses, &mut sockets, auth_key)?;
    let native_meta = add_native_socket(&config, counts.native_count, &mut socket_addresses, &mut sockets, auth_key)?;
    let wasm_wt_meta = add_wasm_wt_socket(&config, counts.wasm_wt_count, &mut socket_addresses, &mut sockets, auth_key)?;
    let wasm_ws_meta = add_wasm_ws_socket(&config, counts.wasm_ws_count, &mut socket_addresses, &mut sockets, auth_key)?;

    let connect_metas = ConnectMetas {
        memory: memory_meta,
        native: native_meta,
        wasm_wt: wasm_wt_meta,
        wasm_ws: wasm_ws_meta,
    };

    // save final addresses
    let server_config = ServerSetupConfig {
        current_time: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default(),
        max_clients,
        protocol_id: config.protocol_id,
        socket_addresses,
        authentication: ServerAuthentication::Secure { private_key: *auth_key },
    };

    // construct server
    let server = RenetServer::new(connection_config);
    let server_transport = NetcodeServerTransport::new_with_sockets(server_config, sockets)
        .map_err(|err| format!("failed constructing renet2 netcode server transport: {err:?}"))?;

    Ok((server, server_transport, connect_metas))
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet2 server with arbitrary combinations of memory/native/wasm transports.
///
/// The server's auth key will be randomly generated. On WASM targets (e.g. for local-player games in browser) the
/// auth key will be set to the current time as nanoseconds.
pub fn setup_combo_renet2_server(
    config: GameServerSetupConfig,
    client_counts: ClientCounts,
    connection_config: ConnectionConfig,
) -> Result<(RenetServer, NetcodeServerTransport, ConnectMetas), String> {
    let auth_key: [u8; 32] = {
        // We assume this is only used for local-player on web.
        #[cfg(target_family = "wasm")]
        {
            let wasm_count = client_counts.wasm_wt_count + client_counts.wasm_ws_count;
            if client_counts.native_count > 0 || wasm_count > 0 {
                return Err(format!(
                    "aborting game app networking construction; target family is WASM where only in-memory \
                    transports are permitted, but found other transport requests (memory: {:?}, native: {:?}, wasm: {:?})",
                    client_counts.memory_clients, client_counts.native_count, wasm_count
                ));
            }

            let time: [u8; 16] = wasm_timer::SystemTime::now()
                .duration_since(wasm_timer::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                .to_le_bytes();
            let mut key: [u8; 32] = Default::default();
            key[..16].clone_from_slice(&time);
            key
        }

        #[cfg(not(target_family = "wasm"))]
        renet2_netcode::generate_random_bytes::<32>()
    };

    setup_combo_renet2_server_with_key(config, client_counts, connection_config, &auth_key)
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet2 server with arbitrary combinations of memory/native/wasm transports.
///
/// Inserts [`RenetServer`] and [`NetcodeServerTransport`] to `world` as resources.
#[cfg(feature = "bevy")]
pub fn setup_combo_renet2_server_in_bevy_with_key(
    server_world: &mut bevy_ecs::prelude::World,
    config: GameServerSetupConfig,
    counts: ClientCounts,
    auth_key: &[u8; 32],
    connection_config: ConnectionConfig,
) -> Result<ConnectMetas, String> {
    let (server, server_transport, connect_metas) = setup_combo_renet2_server_with_key(config, counts, connection_config, auth_key)?;

    server_world.insert_resource(server);
    server_world.insert_resource(server_transport);

    Ok(connect_metas)
}

//-------------------------------------------------------------------------------------------------------------------

/// Sets up a renet2 server with arbitrary combinations of memory/native/wasm transports.
///
/// The server's auth key will be randomly generated. On WASM targets (e.g. for local-player games in browser) the
/// auth key will be set to the current time as nanoseconds.
///
/// Inserts [`RenetServer`] and [`NetcodeServerTransport`] to `world` as resources.
#[cfg(feature = "bevy")]
pub fn setup_combo_renet2_server_in_bevy(
    server_world: &mut bevy_ecs::prelude::World,
    config: GameServerSetupConfig,
    counts: ClientCounts,
    connection_config: ConnectionConfig,
) -> Result<ConnectMetas, String> {
    let (server, server_transport, connect_metas) = setup_combo_renet2_server(config, counts, connection_config)?;

    server_world.insert_resource(server);
    server_world.insert_resource(server_transport);

    Ok(connect_metas)
}

//-------------------------------------------------------------------------------------------------------------------
