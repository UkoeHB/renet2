use bevy::prelude::*;
use bevy_renet2::prelude::{ChannelConfig, SendType};
use bevy_replicon::prelude::{Channel, RepliconChannels};
use std::time::Duration;

//-------------------------------------------------------------------------------------------------------------------

/// External trait for [`RepliconChannels`] to provide convenient conversion into renet2 channel configs.
pub trait RenetChannelsExt {
    /// Returns server channel configs that can be used to create [`ConnectionConfig`](crate::renet2::ConnectionConfig).
    fn server_configs(&self) -> Vec<ChannelConfig>;

    /// Same as [`RenetChannelsExt::server_configs`], but for clients.
    fn client_configs(&self) -> Vec<ChannelConfig>;
}

impl RenetChannelsExt for RepliconChannels {
    /// Returns server channel configs that can be used to create [`ConnectionConfig`](crate::renet2::ConnectionConfig).
    ///
    /// - [`SendType::ReliableUnordered::resend_time`] and [`SendType::ReliableOrdered::resend_time`] will be set to 300 ms.
    /// - [`ChannelConfig::max_memory_usage_bytes`] will be set to `5 * 1024 * 1024`.
    ///
    /// You can configure these parameters after creation. However, do not change [`SendType`], as Replicon relies
    /// on its defined delivery guarantees.
    ///
    /// # Examples
    ///
    /// Configure event channels using
    /// [`RemoteEventRegistry`](bevy_replicon::shared::event::remote_event_registry::RemoteEventRegistry):
    ///
    /// ```
    /// # use bevy::prelude::*;
    /// # use bevy_replicon::{prelude::*, shared::event::remote_event_registry::RemoteEventRegistry};
    /// # use bevy_replicon_renet2::RenetChannelsExt;
    /// # let channels = RepliconChannels::default();
    /// # let registry = RemoteEventRegistry::default();
    /// fn init(channels: Res<RepliconChannels>, event_registry: Res<RemoteEventRegistry>) {
    ///     let mut server_configs = channels.server_configs();
    ///     let fire_id = event_registry.server_channel::<Fire>().unwrap();
    ///     let fire_channel = &mut server_configs[fire_id];
    ///     fire_channel.max_memory_usage_bytes = 2048;
    ///     // Use `server_configs` to create `RenetServer`.
    /// }
    ///
    /// #[derive(Event)]
    /// struct Fire;
    /// ```
    ///
    /// Configure replication channels:
    ///
    /// ```
    /// # use bevy::prelude::*;
    /// # use bevy_replicon::{prelude::*, shared::backend::replicon_channels::ServerChannel};
    /// # use bevy_replicon_renet2::RenetChannelsExt;
    /// # let channels = RepliconChannels::default();
    /// let mut server_configs = channels.server_configs();
    /// let channel = &mut server_configs[ServerChannel::Updates as usize];
    /// channel.max_memory_usage_bytes = 4090;
    /// ```
    fn server_configs(&self) -> Vec<ChannelConfig> {
        let channels = self.server_channels();
        if channels.len() > u8::MAX as usize {
            panic!("number of server channels shouldn't exceed `u8::MAX`");
        }

        create_configs(channels)
    }

    fn client_configs(&self) -> Vec<ChannelConfig> {
        let channels = self.client_channels();
        if channels.len() > u8::MAX as usize {
            panic!("number of client channels shouldn't exceed `u8::MAX`");
        }

        create_configs(channels)
    }
}

//-------------------------------------------------------------------------------------------------------------------

/// Converts Replicon channels into renet2 channel configs.
fn create_configs(channels: &[Channel]) -> Vec<ChannelConfig> {
    let mut channel_configs = Vec::with_capacity(channels.len());
    for (index, &channel) in channels.iter().enumerate() {
        let send_type = match channel {
            Channel::Unreliable => SendType::Unreliable,
            Channel::Unordered => SendType::ReliableUnordered {
                resend_time: Duration::from_millis(300),
            },
            Channel::Ordered => SendType::ReliableOrdered {
                resend_time: Duration::from_millis(300),
            },
        };
        let config = ChannelConfig {
            channel_id: index as u8,
            max_memory_usage_bytes: 5 * 1024 * 1024,
            send_type,
        };

        log::debug!("creating channel config `{config:?}`");
        channel_configs.push(config);
    }
    channel_configs
}

//-------------------------------------------------------------------------------------------------------------------
