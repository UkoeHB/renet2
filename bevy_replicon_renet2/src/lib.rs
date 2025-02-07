#![cfg_attr(docsrs, feature(doc_auto_cfg))]
/*!
Provides integration for [`bevy_replicon`](https://docs.rs/bevy_replicon) for `bevy_renet2`.

# Getting started

This guide assumes that you have already read [quick start guide](https://docs.rs/bevy_replicon#quick-start) from `bevy_replicon`.

All Renet API is re-exported from this plugin, you don't need to include `bevy_renet` or `renet` to your `Cargo.toml`.

Renet by default uses the netcode transport which is re-exported by the `transport` feature. If you want to use other transports, you can disable it.

## Initialization

Add [`RepliconRenetPlugins`] along with [`RepliconPlugins`](bevy_replicon::prelude::RepliconPlugins):

```
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_replicon_renet2::RepliconRenetPlugins;

let mut app = App::new();
app.add_plugins((MinimalPlugins, RepliconPlugins, RepliconRenetPlugins));
```

Plugins in [`RepliconRenetPlugins`] automatically add `renet2` plugins, you don't need to add them.

If the `transport` feature is enabled, netcode plugins will also be automatically added.

## Server and client creation

To connect to the server or create it, you need to initialize the
[`RenetClient`](renet2::RenetClient) and `renet2_netcode::NetcodeClientTransport` **or**
[`RenetServer`](renet2::RenetServer) and `renet2_netcode::NetcodeServerTransport` resources from Renet.

Never insert client and server resources in the same app for single-player, it will cause a replication loop.

This crate provides the [`RenetChannelsExt`] extension trait to conveniently convert channels
from the [`RepliconChannels`] resource into renet2 channels.
When creating a server or client you need to use a [`ConnectionConfig`](renet2::ConnectionConfig)
from [`renet2`], which can be initialized like this:

```
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_replicon_renet2::{renet2::ConnectionConfig, RenetChannelsExt, RepliconRenetPlugins};

# let mut app = App::new();
# app.add_plugins(RepliconPlugins);
let channels = app.world().resource::<RepliconChannels>();
let connection_config = ConnectionConfig::from_channels(
    channels.get_server_configs(),
    channels.get_client_configs(),
);
```

For a full example of how to initialize a server or client see the example in the
repository.
*/

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
