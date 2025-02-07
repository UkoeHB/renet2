//! A simple demo to showcase how player could send inputs to move the square and server replicates position back.
//! Also demonstrates the single-player and how sever also could be a player.
//!
//! Use: cargo run --example simple_box -- single-player   (or client/server)

use std::{
    error::Error,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    time::SystemTime,
};

use bevy::{
    color::palettes::css::LIME,
    prelude::*,
    winit::{UpdateMode::Continuous, WinitSettings},
};
use bevy_replicon::prelude::*;
use bevy_replicon_renet2::{
    netcode::{
        ClientAuthentication, NativeSocket, NetcodeClientTransport, NetcodeServerTransport, ServerAuthentication, ServerSetupConfig,
    },
    renet2::{ConnectionConfig, RenetClient, RenetServer},
    RenetChannelsExt, RepliconRenetPlugins,
};
use clap::Parser;
use serde::{Deserialize, Serialize};

fn main() {
    App::new()
        .init_resource::<Cli>() // Parse CLI before creating window.
        // Makes the server/client update continuously even while unfocused.
        .insert_resource(WinitSettings {
            focused_mode: Continuous,
            unfocused_mode: Continuous,
        })
        .add_plugins((DefaultPlugins, RepliconPlugins, RepliconRenetPlugins, SimpleBoxPlugin))
        .run();
}

struct SimpleBoxPlugin;

impl Plugin for SimpleBoxPlugin {
    fn build(&self, app: &mut App) {
        app.replicate::<PlayerPosition>()
            .replicate::<PlayerColor>()
            .add_client_event::<MoveDirection>(ChannelKind::Ordered)
            .add_systems(Startup, (Self::read_cli.map(Result::unwrap), Self::spawn_camera))
            .add_systems(
                Update,
                (
                    Self::apply_movement.run_if(server_or_singleplayer), // Runs only on the server or a single player.
                    (Self::draw_boxes, Self::read_input),
                ),
            )
            .add_observer(Self::handle_client_connected)
            .add_observer(Self::handle_client_disconnected);
    }
}

impl SimpleBoxPlugin {
    fn read_cli(mut commands: Commands, cli: Res<Cli>, channels: Res<RepliconChannels>) -> Result<(), Box<dyn Error>> {
        match *cli {
            Cli::SinglePlayer => {
                commands.spawn(PlayerBundle::new(ClientId::SERVER, Vec2::ZERO, LIME.into()));
            }
            Cli::Server { port } => {
                let server = RenetServer::new(ConnectionConfig::from_channels(
                    channels.get_server_configs(),
                    channels.get_client_configs(),
                ));

                let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?;
                let public_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
                let socket = UdpSocket::bind(public_addr)?;
                let server_config = ServerSetupConfig {
                    current_time,
                    max_clients: 10,
                    protocol_id: PROTOCOL_ID,
                    authentication: ServerAuthentication::Unsecure,
                    socket_addresses: vec![vec![public_addr]],
                };
                let transport = NetcodeServerTransport::new(server_config, NativeSocket::new(socket).unwrap())?;

                commands.insert_resource(server);
                commands.insert_resource(transport);

                commands.spawn((
                    Text::new("Server"),
                    TextFont {
                        font_size: 30.0,
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));
                commands.spawn(PlayerBundle::new(ClientId::SERVER, Vec2::ZERO, LIME.into()));
            }
            Cli::Client { port, ip } => {
                let client = RenetClient::new(
                    ConnectionConfig::from_channels(channels.get_server_configs(), channels.get_client_configs()),
                    false,
                );

                let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?;
                let client_id = current_time.as_millis() as u64;
                let server_addr = SocketAddr::new(ip, port);
                let socket = UdpSocket::bind((ip, 0))?;
                let authentication = ClientAuthentication::Unsecure {
                    client_id,
                    protocol_id: PROTOCOL_ID,
                    socket_id: 0,
                    server_addr,
                    user_data: None,
                };
                let transport = NetcodeClientTransport::new(current_time, authentication, NativeSocket::new(socket).unwrap())?;

                commands.insert_resource(client);
                commands.insert_resource(transport);

                commands.spawn((
                    Text::new(format!("Client: {client_id:?}")),
                    TextFont {
                        font_size: 30.0,
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));
            }
        }

        Ok(())
    }

    fn spawn_camera(mut commands: Commands) {
        commands.spawn(Camera2d::default());
    }

    /// Logs server events and spawns a new player whenever a client connects.
    fn handle_client_connected(event: Trigger<ClientConnected>, mut commands: Commands) {
        let ClientConnected { client_id } = event.event();
        info!("{client_id:?} connected");
        // Generate pseudo random color from client id.
        let r = ((client_id.get() % 23) as f32) / 23.0;
        let g = ((client_id.get() % 27) as f32) / 27.0;
        let b = ((client_id.get() % 39) as f32) / 39.0;
        commands.spawn(PlayerBundle::new(*client_id, Vec2::ZERO, Color::srgb(r, g, b)));
    }

    /// Logs server events and spawns a new player whenever a client connects.
    fn handle_client_disconnected(event: Trigger<ClientDisconnected>) {
        let ClientDisconnected { client_id, reason } = event.event();
        info!("{client_id:?} disconnected: {reason}");
    }

    fn draw_boxes(mut gizmos: Gizmos, players: Query<(&PlayerPosition, &PlayerColor)>) {
        for (position, color) in &players {
            gizmos.rect(
                Isometry3d::new(Vec3::new(position.x, position.y, 0.0), Quat::IDENTITY),
                Vec2::ONE * 50.0,
                color.0,
            );
        }
    }

    /// Reads player inputs and sends [`MoveDirection`] events.
    fn read_input(mut move_events: EventWriter<MoveDirection>, input: Res<ButtonInput<KeyCode>>) {
        let mut direction = Vec2::ZERO;
        if input.pressed(KeyCode::ArrowRight) {
            direction.x += 1.0;
        }
        if input.pressed(KeyCode::ArrowLeft) {
            direction.x -= 1.0;
        }
        if input.pressed(KeyCode::ArrowUp) {
            direction.y += 1.0;
        }
        if input.pressed(KeyCode::ArrowDown) {
            direction.y -= 1.0;
        }
        if direction != Vec2::ZERO {
            move_events.send(MoveDirection(direction.normalize_or_zero()));
        }
    }

    /// Mutates [`PlayerPosition`] based on [`MoveDirection`] events.
    ///
    /// Fast-paced games usually you don't want to wait until server send a position back because of the latency.
    /// But this example just demonstrates simple replication concept.
    fn apply_movement(
        time: Res<Time>,
        mut move_events: EventReader<FromClient<MoveDirection>>,
        mut players: Query<(&Player, &mut PlayerPosition)>,
    ) {
        const MOVE_SPEED: f32 = 300.0;
        for FromClient { client_id, event } in move_events.read() {
            info!("received event {event:?} from {client_id:?}");
            for (player, mut position) in &mut players {
                if *client_id == player.0 {
                    **position += event.0 * time.delta_secs() * MOVE_SPEED;
                }
            }
        }
    }
}

const PORT: u16 = 5000;
const PROTOCOL_ID: u64 = 0;

#[derive(Parser, PartialEq, Resource)]
enum Cli {
    SinglePlayer,
    Server {
        #[arg(short, long, default_value_t = PORT)]
        port: u16,
    },
    Client {
        #[arg(short, long, default_value_t = Ipv4Addr::LOCALHOST.into())]
        ip: IpAddr,

        #[arg(short, long, default_value_t = PORT)]
        port: u16,
    },
}

impl Default for Cli {
    fn default() -> Self {
        Self::parse()
    }
}

#[derive(Bundle)]
struct PlayerBundle {
    player: Player,
    position: PlayerPosition,
    color: PlayerColor,
    replicated: Replicated,
}

impl PlayerBundle {
    fn new(client_id: ClientId, position: Vec2, color: Color) -> Self {
        Self {
            player: Player(client_id),
            position: PlayerPosition(position),
            color: PlayerColor(color),
            replicated: Replicated,
        }
    }
}

/// Contains the client ID of a player.
#[derive(Component, Serialize, Deserialize)]
struct Player(ClientId);

#[derive(Component, Deserialize, Serialize, Deref, DerefMut)]
struct PlayerPosition(Vec2);

#[derive(Component, Deserialize, Serialize)]
struct PlayerColor(Color);

/// A movement event for the controlled box.
#[derive(Debug, Default, Deserialize, Event, Serialize)]
struct MoveDirection(Vec2);
