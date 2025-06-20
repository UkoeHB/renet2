#[cfg(feature = "netcode")]
use crate::netcode::NetcodeServerPlugin;
use crate::renet2::{RenetReceive, RenetSend, RenetServer, RenetServerPlugin};
use bevy::prelude::*;
use bevy_renet2::prelude::ServerEvent;
use bevy_replicon::{
    prelude::*,
    shared::backend::connected_client::{NetworkId, NetworkIdMap},
};

pub struct RepliconRenetServerPlugin;

impl Plugin for RepliconRenetServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RenetServerPlugin)
            .configure_sets(PreUpdate, ServerSet::ReceivePackets.after(RenetReceive))
            .configure_sets(PostUpdate, ServerSet::SendPackets.before(RenetSend))
            .add_observer(Self::disconnect_client)
            .add_systems(
                PreUpdate,
                (
                    Self::set_running.run_if(resource_added::<RenetServer>),
                    Self::set_stopped.run_if(resource_removed::<RenetServer>),
                    (Self::receive_packets, Self::process_server_events).run_if(resource_exists::<RenetServer>),
                )
                    .chain()
                    .in_set(ServerSet::ReceivePackets),
            )
            .add_systems(
                PostUpdate,
                Self::send_packets
                    .in_set(ServerSet::SendPackets)
                    .run_if(resource_exists::<RenetServer>),
            );

        #[cfg(feature = "netcode")]
        app.add_plugins(NetcodeServerPlugin);
    }
}

impl RepliconRenetServerPlugin {
    fn set_running(mut server: ResMut<RepliconServer>) {
        server.set_running(true);
    }

    fn set_stopped(mut server: ResMut<RepliconServer>) {
        server.set_running(false);
    }

    fn process_server_events(mut commands: Commands, mut server_events: EventReader<ServerEvent>, network_map: Res<NetworkIdMap>) {
        for event in server_events.read() {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    let network_id = NetworkId::new(*client_id);
                    let client_entity = commands
                        .spawn((
                            ConnectedClient {
                                // From https://github.com/UkoeHB/renet2/blob/main/renet2/src/packet.rs#L7
                                max_size: 1200,
                            },
                            network_id,
                        ))
                        .id();
                    log::debug!("spawning client `{client_entity}` with `{network_id:?}`");
                }
                ServerEvent::ClientDisconnected { client_id, reason } => {
                    let network_id = NetworkId::new(*client_id);
                    if let Some(&client_entity) = network_map.get(&network_id) {
                        // Entity could have been despawned by user.
                        commands.entity(client_entity).despawn();
                        log::debug!("despawning client `{client_entity}` with `{network_id:?}`: {reason}");
                    }
                }
            };
        }
    }

    fn receive_packets(
        channels: Res<RepliconChannels>,
        mut renet_server: ResMut<RenetServer>,
        mut replicon_server: ResMut<RepliconServer>,
        mut clients: Query<(Entity, &NetworkId, &mut NetworkStats)>,
    ) {
        for (client_entity, network_id, mut stats) in &mut clients {
            for channel_id in 0..channels.client_channels().len() as u8 {
                while let Some(message) = renet_server.receive_message(network_id.get(), channel_id) {
                    log::trace!("forwarding {} received bytes over channel {channel_id}", message.len());
                    replicon_server.insert_received(client_entity, channel_id, message);
                }
            }

            // Renet events reading runs in parallel, so the client might have been disconnected.
            if let Ok(info) = renet_server.network_info(network_id.get()) {
                stats.rtt = info.rtt;
                stats.packet_loss = info.packet_loss;
                stats.sent_bps = info.bytes_sent_per_second;
                stats.received_bps = info.bytes_received_per_second;
            }
        }
    }

    fn send_packets(mut renet_server: ResMut<RenetServer>, mut replicon_server: ResMut<RepliconServer>, clients: Query<&NetworkId>) {
        for (client_entity, channel_id, message) in replicon_server.drain_sent() {
            log::trace!("forwarding {} sent bytes over channel {channel_id}", message.len());
            let network_id = clients
                .get(client_entity)
                .expect("messages should be sent only to connected clients");
            renet_server.send_message(network_id.get(), channel_id as u8, message)
        }
    }
    fn disconnect_client(trigger: Trigger<OnRemove, ConnectedClient>, server: Option<ResMut<RenetServer>>, clients: Query<&NetworkId>) {
        if let Some(mut server) = server {
            log::debug!("disconnecting despawned client `{}`", trigger.target());

            let network_id = clients.get(trigger.target()).expect("inserted on connection");
            server.disconnect(network_id.get());
        }
    }
}
