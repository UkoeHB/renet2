use bevy::prelude::*;
use bevy_renet2::prelude::{RenetReceive, RenetSend, RenetServer, RenetServerPlugin, ServerEvent};
use bevy_replicon::{
    prelude::*,
    shared::backend::connected_client::{NetworkId, NetworkIdMap},
};

/// Adds Renet as the server messaging backend.
///
/// Initializes [`RenetServerPlugin`] and the systems that pass data between [`RenetServer`]
/// and [`ServerMessages`], update the [`ServerState`], and translate Renet's server events into Replicon's.
pub struct RepliconRenetServerPlugin;

impl Plugin for RepliconRenetServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RenetServerPlugin)
            .configure_sets(PreUpdate, ServerSystems::ReceivePackets.after(RenetReceive))
            .configure_sets(PostUpdate, ServerSystems::SendPackets.before(RenetSend))
            .add_observer(disconnect_client)
            .add_systems(
                PreUpdate,
                (
                    set_running.run_if(resource_added::<RenetServer>),
                    set_stopped.run_if(resource_removed::<RenetServer>),
                    (receive_packets, process_server_events).run_if(resource_exists::<RenetServer>),
                )
                    .in_set(ServerSystems::ReceivePackets),
            )
            .add_systems(
                PostUpdate,
                (
                    send_packets
                        .in_set(ServerSystems::SendPackets)
                        .run_if(resource_exists::<RenetServer>),
                    // Run after sending to let clients receive messages before disconnecting.
                    disconnect_by_request.after(RenetSend),
                ),
            );

        #[cfg(feature = "netcode")]
        app.add_plugins(bevy_renet2::netcode::NetcodeServerPlugin);
        #[cfg(feature = "steam")]
        app.add_plugins(bevy_renet2::steam::SteamServerPlugin);
    }
}

fn set_running(mut state: ResMut<NextState<ServerState>>) {
    state.set(ServerState::Running);
}

fn set_stopped(mut state: ResMut<NextState<ServerState>>) {
    state.set(ServerState::Stopped);
}

fn process_server_events(mut commands: Commands, mut server_events: MessageReader<ServerEvent>, network_map: Res<NetworkIdMap>) {
    for event in server_events.read() {
        match event {
            ServerEvent::ClientConnected { client_id } => {
                let network_id = NetworkId::new(*client_id);
                let client_entity = commands
                    .spawn((
                        ConnectedClient {
                            // From https://github.com/lucaspoffo/renet/blob/master/renet/src/packet.rs#L7
                            max_size: 1200,
                        },
                        network_id,
                    ))
                    .id();
                debug!("spawning client `{client_entity}` with `{network_id:?}`");
            }
            ServerEvent::ClientDisconnected { client_id, reason } => {
                let network_id = NetworkId::new(*client_id);
                if let Some(&client_entity) = network_map.get(&network_id) {
                    // Entity could have been despawned by user.
                    commands.entity(client_entity).despawn();
                    debug!("despawning client `{client_entity}` with `{network_id:?}`: {reason}");
                }
            }
        }
    }
}

fn receive_packets(
    channels: Res<RepliconChannels>,
    mut server: ResMut<RenetServer>,
    mut messages: ResMut<ServerMessages>,
    mut clients: Query<(Entity, &NetworkId, &mut ClientStats)>,
) {
    for (client_entity, network_id, mut stats) in &mut clients {
        for channel_id in 0..channels.client_channels().len() as u8 {
            while let Some(message) = server.receive_message(network_id.get(), channel_id) {
                trace!("forwarding {} received bytes over channel {channel_id}", message.len());
                messages.insert_received(client_entity, channel_id, message);
            }
        }

        // Renet events reading runs in parallel, so the client might have been disconnected.
        if let Ok(info) = server.network_info(network_id.get()) {
            stats.rtt = info.rtt;
            stats.packet_loss = info.packet_loss;
            stats.sent_bps = info.bytes_sent_per_second;
            stats.received_bps = info.bytes_received_per_second;
        }
    }
}

fn send_packets(mut server: ResMut<RenetServer>, mut messages: ResMut<ServerMessages>, clients: Query<&NetworkId>) {
    for (client_entity, channel_id, message) in messages.drain_sent() {
        trace!("forwarding {} sent bytes over channel {channel_id}", message.len());
        let network_id = clients
            .get(client_entity)
            .expect("messages should be sent only to connected clients");
        server.send_message(network_id.get(), channel_id as u8, message)
    }
}

fn disconnect_by_request(mut commands: Commands, mut disconnects: MessageReader<DisconnectRequest>) {
    for disconnect in disconnects.read() {
        debug!("despawning client `{}` by disconnect request", disconnect.client);
        commands.entity(disconnect.client).despawn();
    }
}

fn disconnect_client(remove: On<Remove, ConnectedClient>, server: Option<ResMut<RenetServer>>, clients: Query<&NetworkId>) {
    if let Some(mut server) = server {
        debug!("disconnecting despawned client `{}`", remove.entity);

        let network_id = clients.get(remove.entity).expect("inserted on connection");
        server.disconnect(network_id.get());
    }
}
