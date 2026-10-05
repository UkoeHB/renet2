pub use renet2_netcode::*;

use renet2::{RenetClient, RenetServer};

use bevy_app::{AppExit, prelude::*};
use bevy_ecs::prelude::*;
use bevy_time::prelude::*;

use crate::prelude::{RenetClientPlugin, RenetReceive, RenetSend, RenetServerPlugin, client_should_update};

pub struct NetcodeServerPlugin;

pub struct NetcodeClientPlugin;

impl Plugin for NetcodeServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NetcodeTransportError>();

        app.add_systems(
            PreUpdate,
            Self::update_system
                .in_set(RenetReceive)
                .run_if(resource_exists::<NetcodeServerTransport>)
                .run_if(resource_exists::<RenetServer>)
                .after(RenetServerPlugin::update_system)
                .before(RenetServerPlugin::emit_server_events_system),
        )
        .add_systems(
            PostUpdate,
            Self::send_packets
                .in_set(RenetSend)
                .run_if(resource_exists::<NetcodeServerTransport>)
                .run_if(resource_exists::<RenetServer>),
        )
        .add_systems(
            Last,
            Self::disconnect_on_exit
                .run_if(resource_exists::<NetcodeServerTransport>)
                .run_if(resource_exists::<RenetServer>),
        )
        .add_observer(Self::disconnect_on_removed);
    }
}

impl NetcodeServerPlugin {
    pub fn update_system(
        mut transport: ResMut<NetcodeServerTransport>,
        mut server: ResMut<RenetServer>,
        time: Res<Time<Real>>,
        mut transport_errors: MessageWriter<NetcodeTransportError>,
    ) {
        if let Err(e) = transport.update(time.delta(), &mut server) {
            // TODO: This does not indicate which server socket the error came from.
            for error in e {
                transport_errors.write(error);
            }
        }
    }

    pub fn send_packets(mut transport: ResMut<NetcodeServerTransport>, mut server: ResMut<RenetServer>) {
        transport.send_packets(&mut server);
    }

    pub fn disconnect_on_exit(
        exit: MessageReader<AppExit>,
        mut transport: ResMut<NetcodeServerTransport>,
        mut server: ResMut<RenetServer>,
    ) {
        if !exit.is_empty() {
            transport.disconnect_all(&mut server);
        }
    }

    pub fn disconnect_on_removed(
        _: On<Remove, (NetcodeServerTransport, RenetServer)>,
        transport: Option<ResMut<NetcodeServerTransport>>,
        server: Option<ResMut<RenetServer>>,
    ) {
        if let Some(mut server) = server
            && let Some(mut transport) = transport
        {
            transport.disconnect_all(&mut server);
        }
    }
}

impl Plugin for NetcodeClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NetcodeTransportError>();

        app.add_systems(
            PreUpdate,
            Self::update_system
                .in_set(RenetReceive)
                .run_if(resource_exists::<NetcodeClientTransport>)
                .run_if(client_should_update())
                .after(RenetClientPlugin::update_system),
        )
        .add_systems(
            PostUpdate,
            Self::send_packets
                .in_set(RenetSend)
                .run_if(resource_exists::<NetcodeClientTransport>)
                .run_if(client_should_update()),
        )
        .add_systems(
            Last,
            Self::disconnect_on_exit
                .run_if(resource_exists::<NetcodeClientTransport>)
                .run_if(client_should_update()),
        )
        .add_observer(Self::disconnect_on_removed);
    }
}

impl NetcodeClientPlugin {
    pub fn update_system(
        mut transport: ResMut<NetcodeClientTransport>,
        mut client: ResMut<RenetClient>,
        time: Res<Time<Real>>,
        mut transport_errors: MessageWriter<NetcodeTransportError>,
    ) {
        if let Err(e) = transport.update(time.delta(), &mut client) {
            transport_errors.write(e);
        }
    }

    pub fn send_packets(
        mut transport: ResMut<NetcodeClientTransport>,
        mut client: ResMut<RenetClient>,
        mut transport_errors: MessageWriter<NetcodeTransportError>,
    ) {
        if let Err(e) = transport.send_packets(&mut client) {
            transport_errors.write(e);
        }
    }

    pub fn disconnect_on_exit(exit: MessageReader<AppExit>, mut transport: ResMut<NetcodeClientTransport>) {
        if !exit.is_empty() {
            transport.disconnect();
        }
    }

    pub fn disconnect_on_removed(_: On<Remove, (NetcodeClientTransport, RenetClient)>, transport: Option<ResMut<NetcodeClientTransport>>) {
        if let Some(mut transport) = transport {
            transport.disconnect();
        }
    }
}
