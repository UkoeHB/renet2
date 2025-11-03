pub use renet2::*;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_time::prelude::*;

use crate::prelude::client_should_update;

/// This system set is where all transports receive messages
///
/// If you want to ensure data has arrived in the [`RenetClient`] or [`RenetServer`], then schedule your
/// system after this set.
///
/// This system set runs in PreUpdate.
#[derive(Debug, SystemSet, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenetReceive;

/// This system set is where all transports send messages
///
/// If you want to ensure your packets have been registered by the [`RenetClient`] or [`RenetServer`], then
/// schedule your system before this set.
///
/// This system set runs in PostUpdate.
#[derive(Debug, SystemSet, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenetSend;

pub struct RenetServerPlugin;

pub struct RenetClientPlugin;

impl Plugin for RenetServerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Messages<ServerEvent>>();
        app.add_systems(PreUpdate, Self::update_system.run_if(resource_exists::<RenetServer>));
        app.add_systems(
            PreUpdate,
            Self::emit_server_events_system
                .in_set(RenetReceive)
                .run_if(resource_exists::<RenetServer>)
                .after(Self::update_system),
        );
    }
}

impl RenetServerPlugin {
    pub fn update_system(mut server: ResMut<RenetServer>, time: Res<Time>) {
        server.update(time.delta());
    }

    pub fn emit_server_events_system(mut server: ResMut<RenetServer>, mut server_events: MessageWriter<ServerEvent>) {
        while let Some(event) = server.get_event() {
            server_events.write(event);
        }
    }
}

impl Plugin for RenetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreUpdate, Self::update_system.run_if(client_should_update()));
    }
}

impl RenetClientPlugin {
    pub fn update_system(mut client: ResMut<RenetClient>, time: Res<Time>) {
        client.update(time.delta());
    }
}
