use bevy_ecs::prelude::*;

use renet2::RenetClient;

pub fn client_connected(client: Option<Res<RenetClient>>) -> bool {
    match client {
        Some(client) => client.is_connected(),
        None => false,
    }
}

pub fn client_disconnected(client: Option<Res<RenetClient>>) -> bool {
    match client {
        Some(client) => client.is_disconnected(),
        None => true,
    }
}

pub fn client_connecting(client: Option<Res<RenetClient>>) -> bool {
    match client {
        Some(client) => client.is_connecting(),
        None => false,
    }
}

pub fn client_just_connected(mut last_connected: Local<bool>, client: Option<Res<RenetClient>>) -> bool {
    let connected = client.map(|client| client.is_connected()).unwrap_or(false);

    let just_connected = !*last_connected && connected;
    *last_connected = connected;
    just_connected
}

pub fn client_just_disconnected(mut last_connected: Local<bool>, client: Option<Res<RenetClient>>) -> bool {
    let disconnected = client.map(|client| client.is_disconnected()).unwrap_or(true);

    let just_disconnected = *last_connected && disconnected;
    *last_connected = !disconnected;
    just_disconnected
}

pub fn client_should_update() -> impl SystemCondition<()> {
    // (just_disconnected || !disconnected) && exists<RenetClient>
    IntoSystem::into_system(
        client_just_disconnected
            .or(not(client_disconnected))
            .and(resource_exists::<RenetClient>),
    )
}
