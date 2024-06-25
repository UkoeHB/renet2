use std::{
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant, SystemTime},
};
use warp::Filter;

use log::{debug, info};
use renet2::{
    transport::{
        BoxedSocket, NativeSocket, NetcodeServerTransport, ServerCertHash, ServerSetupConfig, TransportSocket, WebServerDestination,
        WebTransportServer, WebTransportServerConfig,
    },
    ConnectionConfig, DefaultChannel, RenetServer, ServerEvent,
};
use renetcode2::ServerAuthentication;

struct ClientConnectionInfo {
    native_addr: String,
    wt_dest: WebServerDestination,
    cert_hash: ServerCertHash,
}

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .filter_module("h3::server", log::LevelFilter::Warn)
        .filter_module("h3::server::connection", log::LevelFilter::Warn)
        .init();

    let runtime = tokio::runtime::Runtime::new().unwrap();

    let http_addr: SocketAddr = "127.0.0.1:4433".parse().unwrap();
    let max_clients = 10;

    // Native socket
    let wildcard_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let native_socket = NativeSocket::new(UdpSocket::bind(wildcard_addr).unwrap()).unwrap();

    // WebTransport socket
    let (wt_socket, cert_hash) = {
        let (config, cert_hash) = WebTransportServerConfig::new_selfsigned(wildcard_addr, max_clients);
        (WebTransportServer::new(config, runtime.handle().clone()).unwrap(), cert_hash)
    };

    // Save connection info
    let client_connection_info = ClientConnectionInfo {
        native_addr: native_socket.addr().unwrap().to_string().into(),
        wt_dest: wt_socket.addr().unwrap().into(),
        cert_hash,
    };

    // Setup netcode server transport
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let server_config = ServerSetupConfig {
        current_time,
        max_clients,
        protocol_id: 0,
        socket_addresses: vec![vec![native_socket.addr().unwrap()], vec![wt_socket.addr().unwrap()]],
        authentication: ServerAuthentication::Unsecure,
    };
    let transport = NetcodeServerTransport::new_with_sockets(
        server_config,
        Vec::from([BoxedSocket::new(native_socket), BoxedSocket::new(wt_socket)]),
    )
    .unwrap();
    debug!("transport created");

    // Run HTTP server for clients to get connection info.
    runtime.spawn(async move { run_http_server(http_addr, client_connection_info).await });

    // Run the renet server.
    run_renet_server(transport);
}

async fn run_http_server(http_addr: SocketAddr, client_connection_info: ClientConnectionInfo) {
    let native_addr = client_connection_info.native_addr;
    let wt_dest = client_connection_info.wt_dest;
    let cert_hash = client_connection_info.cert_hash;

    let native = warp::path!("native").map(move || warp::reply::json(&native_addr));

    let cors = warp::cors().allow_any_origin();
    let wasm = warp::path!("wasm")
        .map(move || warp::reply::json(&(&wt_dest, &cert_hash)))
        .with(cors);

    let routes = warp::get().and(native.or(wasm));

    warp::serve(routes).run(http_addr).await;
}

fn run_renet_server(mut transport: NetcodeServerTransport) {
    let mut server: RenetServer = RenetServer::new(ConnectionConfig::default());

    let mut received_messages = vec![];
    let mut last_updated = Instant::now();

    loop {
        debug!("server tick");
        let now = Instant::now();
        let duration = now - last_updated;
        last_updated = now;

        transport.update(duration, &mut server).unwrap();
        debug!("server update");

        while let Some(event) = server.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    info!("Client {} connected.", client_id);
                    server.send_message(client_id, DefaultChannel::Unreliable, b"Welcome to the server!".to_vec());
                }
                ServerEvent::ClientDisconnected { client_id, reason } => {
                    info!("Client {} disconnected: {}", client_id, reason);
                }
            }
        }

        for client_id in server.clients_id() {
            while let Some(message) = server.receive_message(client_id, DefaultChannel::Unreliable) {
                let message = String::from_utf8(message.into()).unwrap();
                info!("Client {} sent text: {}", client_id, message);
                let text = format!("{}: {}", client_id, message);
                received_messages.push(text);
            }
        }

        for text in received_messages.drain(..) {
            server.broadcast_message(DefaultChannel::Unreliable, text.as_bytes().to_vec());
        }

        transport.send_packets(&mut server);
        std::thread::sleep(Duration::from_millis(50));
    }
}
