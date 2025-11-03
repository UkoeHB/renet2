use std::time::SystemTime;

use bevy::prelude::*;
use bevy_renet2::netcode::{
    in_memory_server_addr, new_memory_sockets, ClientAuthentication, MemorySocketClient, NetcodeClientPlugin, NetcodeClientTransport,
    NetcodeServerPlugin, NetcodeServerTransport, ServerAuthentication, ServerSetupConfig,
};
use bevy_renet2::prelude::{ConnectionConfig, DefaultChannel, RenetClient, RenetClientPlugin, RenetServer, RenetServerPlugin};

#[derive(Debug, Default, Resource, PartialEq, Eq, Deref, DerefMut)]
pub struct ServerReceived(Vec<(u64, Vec<u8>)>);

#[derive(Debug, Default, Resource, PartialEq, Eq, Deref, DerefMut)]
pub struct ClientReceived(Vec<Vec<u8>>);

const PROTOCOL_ID: u64 = 0;

fn create_server_transport(num_clients: usize) -> (NetcodeServerTransport, Vec<MemorySocketClient>) {
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let server_config = ServerSetupConfig {
        socket_addresses: vec![vec![in_memory_server_addr()]],
        current_time,
        max_clients: num_clients,
        protocol_id: PROTOCOL_ID,
        authentication: ServerAuthentication::Unsecure,
    };
    let (server_socket, client_sockets) = new_memory_sockets((1..=num_clients as u16).collect(), false, false);

    (NetcodeServerTransport::new(server_config, server_socket).unwrap(), client_sockets)
}

fn create_client_transport(socket: MemorySocketClient) -> NetcodeClientTransport {
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let authentication = ClientAuthentication::Unsecure {
        client_id: socket.id(),
        protocol_id: PROTOCOL_ID,
        socket_id: 0,
        server_addr: in_memory_server_addr(),
        user_data: None,
    };

    NetcodeClientTransport::new(current_time, authentication, socket).unwrap()
}

fn setup_server(app: &mut App, num_clients: usize) -> Vec<MemorySocketClient> {
    let server = RenetServer::new(ConnectionConfig::test());
    let (transport, client_sockets) = create_server_transport(num_clients);

    app.insert_resource(server).insert_resource(transport);

    client_sockets
}

fn setup_client(app: &mut App, socket: MemorySocketClient) {
    let client = RenetClient::new(ConnectionConfig::test(), false);
    let transport = create_client_transport(socket);

    app.insert_resource(client).insert_resource(transport);
}

fn create_server_app(num_clients: usize) -> (App, Vec<MemorySocketClient>) {
    let mut server = App::new();
    let client_sockets = setup_server(&mut server, num_clients);

    server
        .add_plugins((MinimalPlugins, RenetServerPlugin, NetcodeServerPlugin))
        .init_resource::<ServerReceived>()
        .add_systems(Update, |mut server: ResMut<RenetServer>, mut received: ResMut<ServerReceived>| {
            for client_id in server.clients_id() {
                while let Some(packet) = server.receive_message(client_id, DefaultChannel::ReliableOrdered) {
                    received.push((client_id, packet.to_vec()));
                }
            }
        });

    (server, client_sockets)
}

fn create_client_app(socket: MemorySocketClient) -> App {
    let mut client = App::new();
    setup_client(&mut client, socket);

    client
        .add_plugins((MinimalPlugins, RenetClientPlugin, NetcodeClientPlugin))
        .init_resource::<ClientReceived>()
        .add_systems(Update, |mut client: ResMut<RenetClient>, mut received: ResMut<ClientReceived>| {
            while let Some(packet) = client.receive_message(DefaultChannel::ReliableOrdered) {
                received.push(packet.to_vec());
            }
        });

    client
}

fn create_and_connect_apps(num_clients: usize) -> (App, Vec<App>) {
    let (mut server, mut client_sockets) = create_server_app(num_clients);
    let mut clients = Vec::with_capacity(client_sockets.len());

    for socket in client_sockets.drain(..) {
        let mut client = create_client_app(socket);

        while !client.world().resource::<RenetClient>().is_connected() {
            server.update();
            client.update();
        }

        clients.push(client);
    }

    (server, clients)
}

fn client_received(client: &App) -> Vec<Vec<u8>> {
    client.world().resource::<ClientReceived>().0.clone()
}

fn server_received(server: &App) -> Vec<(u64, Vec<u8>)> {
    let mut received = server.world().resource::<ServerReceived>().0.clone();
    received.sort_by_key(|&(client_id, _)| client_id);
    received
}

#[test]
fn simple_transport() {
    /*
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::TRACE.into())
        .from_env().unwrap();
    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(filter)
        .init();
    */

    let (mut server, mut clients) = create_and_connect_apps(1);
    let mut client = clients.pop().unwrap();

    assert!(client.world().resource::<NetcodeClientTransport>().is_connected());

    server.add_systems(Update, |mut server: ResMut<RenetServer>| {
        server.broadcast_message(DefaultChannel::ReliableOrdered, vec![1]);
    });
    server.update();
    client.update();

    assert_eq!(client_received(&client), [[1]]);
    assert_eq!(server_received(&server), []);
}

#[test]
fn multiple_messages_server() {
    let (mut server, mut clients) = create_and_connect_apps(1);
    let mut client = clients.pop().unwrap();

    server.add_systems(Update, |mut server: ResMut<RenetServer>| {
        server.broadcast_message(DefaultChannel::ReliableOrdered, vec![1, 2]);
        server.broadcast_message(DefaultChannel::ReliableOrdered, vec![3]);
    });
    server.update();
    server.update();
    client.update();

    assert_eq!(client_received(&client), [vec![1, 2], vec![3], vec![1, 2], vec![3]]);
    assert_eq!(server_received(&server), []);

    server.update();
    client.update();

    assert_eq!(
        client_received(&client),
        [vec![1, 2], vec![3], vec![1, 2], vec![3], vec![1, 2], vec![3]]
    );
    assert_eq!(server_received(&server), []);
}

#[test]
fn multiple_messages_client() {
    let (mut server, mut clients) = create_and_connect_apps(1);
    let mut client = clients.pop().unwrap();

    client.add_systems(Update, |mut client: ResMut<RenetClient>| {
        client.send_message(DefaultChannel::ReliableOrdered, vec![1, 2]);
        client.send_message(DefaultChannel::ReliableOrdered, vec![3]);
    });
    client.update();
    client.update();
    server.update();

    assert!(client_received(&client).is_empty());
    assert_eq!(
        server_received(&server),
        [(1, vec![1, 2]), (1, vec![3]), (1, vec![1, 2]), (1, vec![3])]
    );

    client.update();
    server.update();

    assert!(client_received(&client).is_empty());
    assert_eq!(
        server_received(&server),
        [
            (1, vec![1, 2]),
            (1, vec![3]),
            (1, vec![1, 2]),
            (1, vec![3]),
            (1, vec![1, 2]),
            (1, vec![3])
        ]
    );
}

#[test]
fn both_directions() {
    let (mut server, mut clients) = create_and_connect_apps(1);
    let mut client = clients.pop().unwrap();

    server.add_systems(Update, |mut server: ResMut<RenetServer>| {
        server.broadcast_message(DefaultChannel::ReliableOrdered, vec![1]);
    });
    client.add_systems(Update, |mut client: ResMut<RenetClient>| {
        client.send_message(DefaultChannel::ReliableOrdered, vec![2]);
    });
    server.update();
    client.update();

    assert_eq!(client_received(&client), [[1]]);
    assert_eq!(server_received(&server), []);

    server.update();

    assert_eq!(client_received(&client), [[1]]);
    assert_eq!(server_received(&server), [(1, vec![2])]);
}

#[test]
fn multiple_clients() {
    let (mut server, mut clients) = create_and_connect_apps(3);
    let mut client3 = clients.pop().unwrap();
    let mut client2 = clients.pop().unwrap();
    let mut client1 = clients.pop().unwrap();

    server.add_systems(Update, |mut server: ResMut<RenetServer>| {
        server.broadcast_message(DefaultChannel::ReliableOrdered, vec![0]);
    });
    client1.add_systems(Update, |mut client: ResMut<RenetClient>| {
        client.send_message(DefaultChannel::ReliableOrdered, vec![1]);
    });
    client2.add_systems(Update, |mut client: ResMut<RenetClient>| {
        client.send_message(DefaultChannel::ReliableOrdered, vec![2]);
    });
    client3.add_systems(Update, |mut client: ResMut<RenetClient>| {
        client.send_message(DefaultChannel::ReliableOrdered, vec![3]);
    });

    server.update();
    client1.update();
    client2.update();
    client3.update();

    assert_eq!(client_received(&client1), [[0]]);
    assert_eq!(client_received(&client2), [[0]]);
    assert_eq!(client_received(&client3), [[0]]);
    assert_eq!(server_received(&server), []);

    server.update();

    assert_eq!(client_received(&client1), [[0]]);
    assert_eq!(client_received(&client2), [[0]]);
    assert_eq!(client_received(&client3), [[0]]);
    assert_eq!(server_received(&server), [(1, vec![1]), (2, vec![2]), (3, vec![3])]);
}

#[test]
fn disconnect_client() {
    let (mut server, mut clients) = create_and_connect_apps(1);
    let mut client = clients.pop().unwrap();

    server.update();
    assert_eq!(server.world().resource::<RenetServer>().clients_id(), vec![1]);
    assert!(client.world().resource::<NetcodeClientTransport>().is_connected());

    client.world_mut().write_message(AppExit::Success);
    client.update();
    server.update();
    client.update();
    server.update();

    assert!(client.world().resource::<RenetClient>().is_disconnected());
    assert!(!client.world().resource::<NetcodeClientTransport>().is_connected());

    assert!(server.world().resource::<RenetServer>().clients_id().is_empty());
}

#[test]
fn disconnect_server() {
    let (mut server, mut clients) = create_and_connect_apps(1);
    let mut client = clients.pop().unwrap();

    assert_eq!(server.world().resource::<RenetServer>().clients_id(), [1]);
    assert!(client.world().resource::<NetcodeClientTransport>().is_connected());

    server.world_mut().write_message(AppExit::Success);
    server.update();
    client.update();
    client.update();

    assert!(server.world().resource::<RenetServer>().clients_id().is_empty());

    assert!(client.world().resource::<RenetClient>().is_disconnected());
    assert!(!client.world().resource::<NetcodeClientTransport>().is_connected());
}

#[test]
fn no_transport() {
    let (mut server, mut clients) = create_and_connect_apps(1);
    let mut client = clients.pop().unwrap();

    server.world_mut().remove_resource::<NetcodeServerTransport>();
    client.world_mut().remove_resource::<NetcodeClientTransport>();

    server.update();
    client.update();
}
