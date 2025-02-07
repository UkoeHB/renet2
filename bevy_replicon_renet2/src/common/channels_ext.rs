use bevy_renet2::prelude::{ChannelConfig, SendType};
use bevy_replicon::prelude::{ChannelKind, RepliconChannel, RepliconChannels};

//-------------------------------------------------------------------------------------------------------------------

/// External trait for [`RepliconChannels`] to provide convenient conversion into renet2 channel configs.
pub trait RenetChannelsExt {
    /// Returns server channel configs that can be used to create [`ConnectionConfig`](renet2::ConnectionConfig).
    fn get_server_configs(&self) -> Vec<ChannelConfig>;

    /// Same as [`RenetChannelsExt::get_server_configs`], but for clients.
    fn get_client_configs(&self) -> Vec<ChannelConfig>;
}

impl RenetChannelsExt for RepliconChannels {
    fn get_server_configs(&self) -> Vec<ChannelConfig> {
        create_configs(self.server_channels(), self.default_max_bytes)
    }

    fn get_client_configs(&self) -> Vec<ChannelConfig> {
        create_configs(self.client_channels(), self.default_max_bytes)
    }
}

//-------------------------------------------------------------------------------------------------------------------

/// Converts replicon channels into renet2 channel configs.
fn create_configs(channels: &[RepliconChannel], default_max_bytes: usize) -> Vec<ChannelConfig> {
    let mut channel_configs = Vec::with_capacity(channels.len());
    for (index, channel) in channels.iter().enumerate() {
        let send_type = match channel.kind {
            ChannelKind::Unreliable => SendType::Unreliable,
            ChannelKind::Unordered => SendType::ReliableUnordered {
                resend_time: channel.resend_time,
            },
            ChannelKind::Ordered => SendType::ReliableOrdered {
                resend_time: channel.resend_time,
            },
        };
        channel_configs.push(ChannelConfig {
            channel_id: index as u8,
            max_memory_usage_bytes: channel.max_bytes.unwrap_or(default_max_bytes),
            send_type,
        });
    }
    channel_configs
}

//-------------------------------------------------------------------------------------------------------------------
