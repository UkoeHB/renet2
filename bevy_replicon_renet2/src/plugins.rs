use bevy::{app::PluginGroupBuilder, prelude::*};

use crate::{RepliconRenetClientPlugin, RepliconRenetServerPlugin};

pub struct RepliconRenetPlugins;

impl PluginGroup for RepliconRenetPlugins {
    fn build(self) -> PluginGroupBuilder {
        let mut builder = PluginGroupBuilder::start::<Self>();

        #[cfg(feature = "client")]
        {
            builder = builder.add(RepliconRenetClientPlugin);
        }

        #[cfg(feature = "server")]
        {
            builder = builder.add(RepliconRenetServerPlugin);
        }

        builder
    }
}
