/*!
Provides integration for [`bevy_replicon`](https://docs.rs/bevy_replicon) for [`bevy_renet2`](https://docs.rs/bevy_renet2).

# Getting started

This guide assumes that you have already read the [quick start guide](https://docs.rs/bevy_replicon#quick-start)
for `bevy_replicon`.

## Modules

Renet2 API is re-exported from this crate under [`renet2`] module. Features from `bevy_renet2` are exposed,
which also re-export the corresponding transport modules. Like in `bevy_renet2`, the netcode
transport is enabled by default.

So you don't need to include `bevy_renet2` or `renet2` in your `Cargo.toml`.

## Plugins

Add [`RepliconRenetPlugins`] along with [`RepliconPlugins`](bevy_replicon::prelude::RepliconPlugins):

```
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_replicon_renet2::RepliconRenetPlugins;

let mut app = App::new();
app.add_plugins((MinimalPlugins, RepliconPlugins, RepliconRenetPlugins));
```

Similar to Replicon, we provide `client` and `server` features. These automatically enable the corresponding
features in `bevy_replicon`.

The plugins in [`RepliconRenetPlugins`] automatically include the `renet2` plugins, so you don't need to add
them manually. When a transport feature is enabled, the netcode plugins will also be added automatically.

## Server and client creation

Just like with regular `bevy_renet2`, you need to create the
[`RenetClient`](renet2::RenetClient) and [`NetcodeClientTransport`](netcode::NetcodeClientTransport) **or**
[`RenetServer`](renet2::RenetServer) and [`NetcodeServerTransport`](netcode::NetcodeServerTransport)
resources from Renet.

This crate will automatically manage their integration with Replicon.

<div class="warning">

Never insert client and server resources in the same app, it will cause a replication loop.
See the Replicon's quick start guide for more details.

</div>

The only Replicon-specific part is channels. You need to get them from the [`RepliconChannels`](bevy_replicon::prelude::RepliconChannels) resource.
This crate provides the [`RenetChannelsExt`] extension trait to conveniently create renet channels from it:

```
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_replicon_renet2::{renet2::ConnectionConfig, RenetChannelsExt};

fn init(channels: Res<RepliconChannels>) {
    let connection_config = ConnectionConfig::from_channels(
        channels.server_configs(),
        channels.client_configs(),
    );

    // Use this config for `RenetServer` or `RenetClient`
}
```

For a full example of how to initialize a server or client see examples in the repository.

<div class="warning">

Channels need to be obtained only **after** registering all replication components and remote events.

</div>

## Replicon conditions

The crate updates the running state of [`RepliconServer`](bevy_replicon::prelude::RepliconServer) and connection state of [`RepliconClient`](bevy_replicon::prelude::RepliconClient)
based on the states of [`RenetServer`](renet2::RenetServer) and [`RenetClient`](renet2::RenetServer)
in [`PreUpdate`](bevy::prelude::PreUpdate).

This means that [Replicon conditions](bevy_replicon::shared::common_conditions) won't work in schedules
like [`Startup`](bevy::prelude::Startup). As a workaround, you can directly check if renet's resources are present. This may be resolved
in the future once we have [observers for resources](https://github.com/bevyengine/bevy/issues/12231)
to immediately react to changes.
*/
#![cfg_attr(docsrs, feature(doc_cfg))]

pub use bevy_renet2::prelude as renet2;

#[cfg(feature = "netcode")]
pub use bevy_renet2::netcode;

#[cfg(feature = "client")]
pub mod client;
pub mod common;
mod plugins;
#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "client")]
pub use client::*;
#[cfg(feature = "server")]
pub use server::*;

pub use common::*;
pub use plugins::*;
