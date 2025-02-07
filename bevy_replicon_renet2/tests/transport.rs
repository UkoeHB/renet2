use std::{
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    time::SystemTime,
};

use bevy::prelude::*;
use bevy_renet2::netcode::{
    ClientAuthentication, NativeSocket, NetcodeClientTransport, NetcodeServerTransport, ServerAuthentication, ServerSetupConfig,
};
use bevy_renet2::prelude::{ConnectionConfig, RenetClient, RenetServer};
use bevy_replicon::prelude::*;
use bevy_replicon_renet2::{RenetChannelsExt, RepliconRenetPlugins};
use serde::{Deserialize, Serialize};

#[test]
fn connect_disconnect() {
    /*
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::DEBUG.into())
        .from_env()
        .unwrap()
        .add_directive("renet2=debug".parse().unwrap())
        .add_directive("renetcode2=debug".parse().unwrap());
    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    */

    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconRenetPlugins,
        ));
    }

    setup(&mut server_app, &mut client_app);

    let mut renet_client = client_app.world_mut().resource_mut::<RenetClient>();
    assert!(renet_client.is_connected());
    renet_client.disconnect();

    client_app.update();
    server_app.update();

    let renet_server = server_app.world().resource::<RenetServer>();
    assert_eq!(renet_server.connected_clients(), 0);

    let connected_clients = server_app.world().resource::<ConnectedClients>();
    assert_eq!(connected_clients.len(), 0);

    let replicon_client = client_app.world_mut().resource_mut::<RepliconClient>();
    assert!(replicon_client.is_disconnected());
}

#[test]
fn replication() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconRenetPlugins,
        ));
    }

    setup(&mut server_app, &mut client_app);

    server_app.world_mut().spawn(Replicated);

    server_app.update();
    client_app.update();

    client_app.world_mut().query::<&Replicated>().single(client_app.world());
}

#[test]
fn server_event() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconRenetPlugins,
        ))
        .add_server_event::<DummyEvent>(ChannelKind::Ordered)
        .finish();
    }

    setup(&mut server_app, &mut client_app);

    server_app.world_mut().send_event(ToClients {
        mode: SendMode::Broadcast,
        event: DummyEvent,
    });

    server_app.update();
    client_app.update();

    let dummy_events = client_app.world().resource::<Events<DummyEvent>>();
    assert_eq!(dummy_events.len(), 1);
}

#[test]
fn client_event() {
    let mut server_app = App::new();
    let mut client_app = App::new();
    for app in [&mut server_app, &mut client_app] {
        app.add_plugins((
            MinimalPlugins,
            RepliconPlugins.set(ServerPlugin {
                tick_policy: TickPolicy::EveryFrame,
                ..Default::default()
            }),
            RepliconRenetPlugins,
        ))
        .add_client_event::<DummyEvent>(ChannelKind::Ordered)
        .finish();
    }

    setup(&mut server_app, &mut client_app);

    client_app.world_mut().send_event(DummyEvent);

    client_app.update();
    server_app.update();

    let client_events = server_app.world().resource::<Events<FromClient<DummyEvent>>>();
    assert_eq!(client_events.len(), 1);
}

fn setup(server_app: &mut App, client_app: &mut App) {
    const CLIENT_ID: u64 = 1;
    let port = setup_server(server_app, 1);
    setup_client(client_app, CLIENT_ID, port);
    wait_for_connection(server_app, client_app);
}

fn setup_client(app: &mut App, client_id: u64, port: u16) {
    let channels = app.world().resource::<RepliconChannels>();

    let server_channels_config = channels.get_server_configs();
    let client_channels_config = channels.get_client_configs();

    let client = RenetClient::new(
        ConnectionConfig::from_channels(server_channels_config, client_channels_config),
        false,
    );
    let transport = create_client_transport(client_id, port);

    app.insert_resource(client).insert_resource(transport);
}

fn setup_server(app: &mut App, max_clients: usize) -> u16 {
    let channels = app.world().resource::<RepliconChannels>();

    let server_channels_config = channels.get_server_configs();
    let client_channels_config = channels.get_client_configs();

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

#[derive(Deserialize, Event, Serialize)]
struct DummyEvent;
