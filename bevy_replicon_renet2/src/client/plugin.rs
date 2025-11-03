use bevy::prelude::*;
use bevy_renet2::prelude::{RenetClient, RenetClientPlugin, RenetReceive, RenetSend};
use bevy_replicon::prelude::*;

/// Adds Renet as the client messaging backend.
///
/// Initializes [`RenetClientPlugin`] and the systems that pass data between [`RenetClient`]
/// and [`ClientMessages`], and update the [`ClientState`].
pub struct RepliconRenetClientPlugin;

impl Plugin for RepliconRenetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RenetClientPlugin)
            .configure_sets(PreUpdate, ClientSystems::ReceivePackets.after(RenetReceive))
            .configure_sets(PostUpdate, ClientSystems::SendPackets.before(RenetSend))
            .add_systems(
                PreUpdate,
                (
                    set_connecting.run_if(resource_added::<RenetClient>),
                    set_connected.run_if(
                        // Ensure we transition from "connecting" to "connected,"
                        // even if the transport reports "connected" right away.
                        in_state(ClientState::Connecting).and(bevy_renet2::prelude::client_connected),
                    ),
                    set_disconnected.run_if(bevy_renet2::prelude::client_just_disconnected),
                    receive_packets.run_if(bevy_renet2::prelude::client_connected),
                )
                    .in_set(ClientSystems::ReceivePackets),
            )
            .add_systems(
                PostUpdate,
                send_packets
                    .in_set(ClientSystems::SendPackets)
                    .run_if(bevy_renet2::prelude::client_connected),
            );

        #[cfg(feature = "netcode")]
        {
            app.add_plugins(bevy_renet2::netcode::NetcodeClientPlugin);
        }
        #[cfg(feature = "steam")]
        {
            app.add_plugins(bevy_renet2::steam::SteamClientPlugin);
        }
    }
}

fn set_connecting(mut state: ResMut<NextState<ClientState>>) {
    state.set(ClientState::Connecting);
}

fn set_connected(mut state: ResMut<NextState<ClientState>>) {
    state.set(ClientState::Connected);
}

fn set_disconnected(mut state: ResMut<NextState<ClientState>>) {
    state.set(ClientState::Disconnected);
}

fn receive_packets(
    channels: Res<RepliconChannels>,
    mut client: ResMut<RenetClient>,
    mut messages: ResMut<ClientMessages>,
    mut stats: ResMut<ClientStats>,
) {
    for channel_id in 0..channels.server_channels().len() as u8 {
        while let Some(message) = client.receive_message(channel_id) {
            log::trace!("forwarding {} received bytes over channel {channel_id}", message.len());
            messages.insert_received(channel_id, message);
        }
    }

    stats.rtt = client.rtt();
    stats.packet_loss = client.packet_loss();
    stats.sent_bps = client.bytes_sent_per_sec();
    stats.received_bps = client.bytes_received_per_sec();
}

fn send_packets(mut client: ResMut<RenetClient>, mut messages: ResMut<ClientMessages>) {
    for (channel_id, message) in messages.drain_sent() {
        log::trace!("forwarding {} sent bytes over channel {channel_id}", message.len());
        client.send_message(channel_id as u8, message)
    }
}
