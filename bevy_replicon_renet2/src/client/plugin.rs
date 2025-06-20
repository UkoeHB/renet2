#[cfg(feature = "netcode")]
use crate::netcode::NetcodeClientPlugin;
use crate::renet2::{RenetClient, RenetClientPlugin, RenetReceive, RenetSend};
use bevy::prelude::*;
use bevy_replicon::prelude::*;

pub struct RepliconRenetClientPlugin;

impl Plugin for RepliconRenetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RenetClientPlugin)
            .configure_sets(PreUpdate, ClientSet::ReceivePackets.after(RenetReceive))
            .configure_sets(PostUpdate, ClientSet::SendPackets.before(RenetSend))
            .add_systems(
                PreUpdate,
                (
                    Self::set_connected.run_if(crate::renet2::client_just_connected),
                    Self::receive_packets.run_if(crate::renet2::client_connected),
                )
                    .chain()
                    .in_set(ClientSet::ReceivePackets),
            )
            .add_systems(
                PostUpdate,
                (
                    (
                        Self::set_connecting.run_if(crate::renet2::client_connecting),
                        Self::set_disconnected.run_if(crate::renet2::client_just_disconnected),
                    )
                        .before(ClientSet::Send),
                    Self::send_packets
                        .in_set(ClientSet::SendPackets)
                        .run_if(crate::renet2::client_connected),
                ),
            );

        #[cfg(feature = "netcode")]
        app.add_plugins(NetcodeClientPlugin);
    }
}

impl RepliconRenetClientPlugin {
    fn set_disconnected(mut client: ResMut<RepliconClient>) {
        client.set_status(RepliconClientStatus::Disconnected);
    }

    fn set_connecting(mut client: ResMut<RepliconClient>) {
        if client.status() != RepliconClientStatus::Connecting {
            client.set_status(RepliconClientStatus::Connecting);
        }
    }

    fn set_connected(mut client: ResMut<RepliconClient>) {
        client.set_status(RepliconClientStatus::Connected);
    }

    fn receive_packets(
        channels: Res<RepliconChannels>,
        mut renet_client: ResMut<RenetClient>,
        mut replicon_client: ResMut<RepliconClient>,
    ) {
        for channel_id in 0..channels.server_channels().len() as u8 {
            while let Some(message) = renet_client.receive_message(channel_id) {
                replicon_client.insert_received(channel_id, message);
            }
        }

        let stats = replicon_client.stats_mut();
        stats.rtt = renet_client.rtt();
        stats.packet_loss = renet_client.packet_loss();
        stats.sent_bps = renet_client.bytes_sent_per_sec();
        stats.received_bps = renet_client.bytes_received_per_sec();
    }

    fn send_packets(mut renet_client: ResMut<RenetClient>, mut replicon_client: ResMut<RepliconClient>) {
        for (channel_id, message) in replicon_client.drain_sent() {
            renet_client.send_message(channel_id as u8, message)
        }
    }
}
