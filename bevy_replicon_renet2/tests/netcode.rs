use std::{
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    time::SystemTime,
};

use bevy::{ecs::schedule::ScheduleLabel, prelude::*, state::app::StatesPlugin};
use bevy_renet2::{
    netcode::{
        ClientAuthentication, NativeSocket, NetcodeClientTransport, NetcodeServerTransport, ServerAuthentication, ServerSetupConfig,
    },
    prelude::{ConnectionConfig, RenetClient, RenetServer},
};
use bevy_replicon::prelude::*;
use bevy_replicon_renet2::{RenetChannelsExt, RepliconRenetPlugins};
use serde::{Deserialize, Serialize};

#[test]
fn connect_disconnect() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
            RepliconRenetPlugins,
        ))
        .finish();
    }

    setup(&mut server_app, &mut client_app);

    let server_state = server_app.world().resource::<State<ServerState>>();
    assert_eq!(*server_state, ServerState::Running);

    let renet_server = server_app.world().resource::<RenetServer>();
    assert_eq!(renet_server.connected_clients(), 1);

    let mut clients = server_app.world_mut().query::<(&ConnectedClient, &AuthorizedClient)>();
    assert_eq!(clients.iter(server_app.world()).len(), 1);

    let client_state = client_app.world().resource::<State<ClientState>>();
    assert_eq!(*client_state, ClientState::Connected);

    let mut renet_client = client_app.world_mut().resource_mut::<RenetClient>();
    assert!(renet_client.is_connected());

    renet_client.disconnect();

    client_app.update();
    server_app.update();

    assert_eq!(clients.iter(server_app.world()).len(), 0);

    let renet_server = server_app.world().resource::<RenetServer>();
    assert_eq!(renet_server.connected_clients(), 0);

    let client_state = client_app.world().resource::<State<ClientState>>();
    assert_eq!(*client_state, ClientState::Disconnected);
}

#[test]
fn disconnect_request() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
            RepliconRenetPlugins,
        ))
        .add_server_message::<Test>(Channel::Ordered)
        .finish();
    }

    setup(&mut server_app, &mut client_app);

    server_app.world_mut().spawn(Replicated);
    server_app.world_mut().write_message(ToClients {
        mode: SendMode::Broadcast,
        message: Test,
    });

    let mut clients = server_app.world_mut().query_filtered::<Entity, With<ConnectedClient>>();
    let client = clients.single(server_app.world()).unwrap();
    server_app.world_mut().write_message(DisconnectRequest { client });

    server_app.update();

    assert_eq!(clients.iter(server_app.world()).len(), 0);

    server_app.update(); // Requires additional update to let transport process the disconnect.
    client_app.update();

    assert!(
        client_app.world().resource::<RenetClient>().is_connected(),
        "renet client disconnects only on the next frame"
    );

    client_app.update();

    let client_state = client_app.world().resource::<State<ClientState>>();
    assert_eq!(*client_state, ClientState::Disconnected);

    let messages = client_app.world().resource::<Messages<Test>>();
    assert_eq!(messages.len(), 1, "last message should be received");

    let mut remote = client_app.world_mut().query::<&Remote>();
    assert_eq!(remote.iter(client_app.world()).len(), 1, "last replication should be received");
}

#[test]
fn server_stop() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
            RepliconRenetPlugins,
        ))
        .add_server_message::<Test>(Channel::Ordered)
        .finish();
    }

    setup(&mut server_app, &mut client_app);

    server_app.world_mut().spawn(Replicated);
    server_app.world_mut().write_message(ToClients {
        mode: SendMode::Broadcast,
        message: Test,
    });

    // In renet, it's necessary to explicitly call disconnect before removing
    // the server resource to let clients receive a disconnect.
    let mut server = server_app.world_mut().resource_mut::<RenetServer>();
    server.disconnect_all();

    server_app.update();
    client_app.update();

    let mut clients = server_app.world_mut().query::<&ConnectedClient>();
    assert_eq!(clients.iter(server_app.world()).len(), 0);

    let server_state = server_app.world().resource::<State<ServerState>>();
    assert_eq!(*server_state, ServerState::Running, "requires resource removal");

    let client_state = client_app.world().resource::<State<ClientState>>();
    assert_eq!(
        *client_state,
        ClientState::Connected,
        "renet client disconnects only on the next frame"
    );

    server_app.world_mut().remove_resource::<RenetServer>();

    server_app.update();
    client_app.update();

    let server_state = client_app.world().resource::<State<ServerState>>();
    assert_eq!(*server_state, ServerState::Stopped);

    let client_state = client_app.world().resource::<State<ClientState>>();
    assert_eq!(*client_state, ClientState::Disconnected);

    let messages = client_app.world().resource::<Messages<Test>>();
    assert!(messages.is_empty(), "message shouldn't be received after stop");

    let mut remote = client_app.world_mut().query::<&Remote>();
    assert_eq!(
        remote.iter(client_app.world()).len(),
        0,
        "replication after stop shouldn't be received"
    );
}

#[test]
fn replication() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
            RepliconRenetPlugins,
        ))
        .finish();
    }

    setup(&mut server_app, &mut client_app);

    server_app.world_mut().spawn(Replicated);

    server_app.update();
    client_app.update();

    let mut remote = client_app.world_mut().query::<&Remote>();
    assert_eq!(remote.iter(client_app.world()).len(), 1);
}

#[test]
fn server_message() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
            RepliconRenetPlugins,
        ))
        .add_server_message::<Test>(Channel::Ordered)
        .finish();
    }

    setup(&mut server_app, &mut client_app);

    server_app.world_mut().write_message(ToClients {
        mode: SendMode::Broadcast,
        message: Test,
    });

    server_app.update();
    client_app.update();

    let messages = client_app.world().resource::<Messages<Test>>();
    assert_eq!(messages.len(), 1);
}

#[test]
fn client_message() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
            RepliconRenetPlugins,
        ))
        .add_client_message::<Test>(Channel::Ordered)
        .finish();
    }

    setup(&mut server_app, &mut client_app);

    client_app.world_mut().write_message(Test);

    client_app.update();
    server_app.update();

    let messages = server_app.world().resource::<Messages<FromClient<Test>>>();
    assert_eq!(messages.len(), 1);
}

fn setup(server_app: &mut App, client_app: &mut App) {
    const CLIENT_ID: u64 = 1;
    let port = setup_server(server_app, 1);
    setup_client(client_app, CLIENT_ID, port);
    wait_for_connection(server_app, client_app);
}

fn setup_client(app: &mut App, client_id: u64, port: u16) {
    let channels = app.world().resource::<RepliconChannels>();

    let server_channels_config = channels.server_configs();
    let client_channels_config = channels.client_configs();

    let client = RenetClient::new(
        ConnectionConfig::from_channels(server_channels_config, client_channels_config),
        false,
    );
    let transport = create_client_transport(client_id, port);

    app.insert_resource(client).insert_resource(transport);
}

fn setup_server(app: &mut App, max_clients: usize) -> u16 {
    let channels = app.world().resource::<RepliconChannels>();

    let server_channels_config = channels.server_configs();
    let client_channels_config = channels.client_configs();

    let server = RenetServer::new(ConnectionConfig::from_channels(server_channels_config, client_channels_config));
    let transport = create_server_transport(max_clients);
    let port = transport.addresses().first().unwrap().port();

    app.insert_resource(server).insert_resource(transport);

    port
}

const PROTOCOL_ID: u64 = 0;

fn create_server_transport(max_clients: usize) -> NetcodeServerTransport {
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let server_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0);
    let socket = UdpSocket::bind(server_addr).expect("localhost should be bindable");
    let public_addr = socket.local_addr().expect("socket should autodetect local address");
    let server_config = ServerSetupConfig {
        current_time,
        max_clients,
        protocol_id: PROTOCOL_ID,
        socket_addresses: vec![vec![public_addr]],
        authentication: ServerAuthentication::Unsecure,
    };

    NetcodeServerTransport::new(server_config, NativeSocket::new(socket).unwrap()).unwrap()
}

fn create_client_transport(client_id: u64, port: u16) -> NetcodeClientTransport {
    let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let ip = Ipv4Addr::LOCALHOST.into();
    let server_addr = SocketAddr::new(ip, port);
    let socket = UdpSocket::bind((ip, 0)).expect("localhost should be bindable");
    let authentication = ClientAuthentication::Unsecure {
        client_id,
        protocol_id: PROTOCOL_ID,
        socket_id: 0,
        server_addr,
        user_data: None,
    };

    NetcodeClientTransport::new(current_time, authentication, NativeSocket::new(socket).unwrap()).unwrap()
}

fn wait_for_connection(server_app: &mut App, client_app: &mut App) {
    loop {
        client_app.update();
        server_app.update();
        if client_app.world().resource::<RenetClient>().is_connected() {
            break;
        }
    }
}

#[derive(Message, Serialize, Deserialize)]
struct Test;
