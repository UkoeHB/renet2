pub use renet2_steam::*;

use ::steamworks::SteamError;
use bevy_app::{AppExit, prelude::*};
use bevy_ecs::prelude::*;
use renet2::{RenetClient, RenetServer};

use crate::prelude::{RenetClientPlugin, RenetReceive, RenetSend, RenetServerPlugin, client_should_update};

pub struct SteamServerPlugin;

pub struct SteamClientPlugin;

#[derive(Debug, Message)]
pub struct SteamTransportError(pub SteamError);

impl std::fmt::Display for SteamTransportError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "{}", self.0)
    }
}

impl Plugin for SteamServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PreUpdate,
            Self::update_system
                .in_set(RenetReceive)
                .run_if(resource_exists::<RenetServer>)
                .after(RenetServerPlugin::update_system)
                .before(RenetServerPlugin::emit_server_events_system),
        );

        app.add_systems(
            PostUpdate,
            (Self::send_packets.in_set(RenetSend), Self::disconnect_on_exit).run_if(resource_exists::<RenetServer>),
        );
    }
}

impl SteamServerPlugin {
    pub fn update_system(mut transport: Option<ResMut<SteamServerTransport>>, mut server: ResMut<RenetServer>) {
        if let Some(transport) = transport.as_mut() {
            transport.update(&mut server);
        }
    }

    pub fn send_packets(mut transport: Option<ResMut<SteamServerTransport>>, mut server: ResMut<RenetServer>) {
        if let Some(transport) = transport.as_mut() {
            transport.send_packets(&mut server);
        }
    }

    pub fn disconnect_on_exit(
        exit: MessageReader<AppExit>,
        mut transport: Option<ResMut<SteamServerTransport>>,
        mut server: ResMut<RenetServer>,
    ) {
        if let Some(transport) = transport.as_mut() {
            if !exit.is_empty() {
                transport.disconnect_all(&mut server, false);
            }
        }
    }
}

impl Plugin for SteamClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SteamTransportError>();

        app.add_systems(
            PreUpdate,
            Self::update_system
                .in_set(RenetReceive)
                .run_if(resource_exists::<SteamClientTransport>)
                .run_if(client_should_update())
                .after(RenetClientPlugin::update_system),
        );
        app.add_systems(
            PostUpdate,
            (Self::send_packets.in_set(RenetSend), Self::disconnect_on_exit)
                .run_if(resource_exists::<SteamClientTransport>)
                .run_if(client_should_update()),
        );
    }
}

impl SteamClientPlugin {
    pub fn update_system(mut transport: ResMut<SteamClientTransport>, mut client: ResMut<RenetClient>) {
        transport.update(&mut client);
    }

    pub fn send_packets(
        mut transport: ResMut<SteamClientTransport>,
        mut client: ResMut<RenetClient>,
        mut transport_errors: MessageWriter<SteamTransportError>,
    ) {
        if let Err(e) = transport.send_packets(&mut client) {
            transport_errors.write(SteamTransportError(e));
        }
    }

    pub fn disconnect_on_exit(exit: MessageReader<AppExit>, mut transport: ResMut<SteamClientTransport>) {
        if !exit.is_empty() {
            transport.disconnect();
        }
    }
}
