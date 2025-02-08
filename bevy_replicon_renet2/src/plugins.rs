use bevy::{app::PluginGroupBuilder, prelude::*};

pub struct RepliconRenetPlugins;

impl PluginGroup for RepliconRenetPlugins {
    fn build(self) -> PluginGroupBuilder {
        let mut builder = PluginGroupBuilder::start::<Self>();

        #[cfg(feature = "client")]
        {
            builder = builder.add(crate::RepliconRenetClientPlugin);
        }

        #[cfg(feature = "server")]
        {
            builder = builder.add(crate::RepliconRenetServerPlugin);
        }

        builder
    }
}
