use crate::common::ConnectionType;

//-------------------------------------------------------------------------------------------------------------------

/// The number of clients that will connect to a server with different connection types.
///
/// Used by [`setup_combo_renet2_server`] to set max-client limits and determine what server sockets are required.
/// Note that we assume clients will not change connection type throughout a game. If you want to allow clients
/// to change connection type, then set each count below equal to the total number of clients.
#[derive(Debug, Default, Clone)]
pub struct ClientCounts {
    /// The ids of in-memory clients that will connect.
    ///
    /// Ids must be in the range `[0, u16::MAX)`.
    pub memory_clients: Vec<u16>,
    /// The number of native clients that will connect.
    pub native_count: usize,
    /// The number of WASM webtransport clients that will connect.
    pub wasm_wt_count: usize,
    /// The number of WASM websocket clients that will connect.
    pub wasm_ws_count: usize,
}

impl ClientCounts {
    /// The `client_id` is used for in-memory clients.
    pub fn add(&mut self, connection: ConnectionType, client_id: u64) {
        match connection {
            ConnectionType::Memory => {
                self.memory_clients
                    .push(u16::try_from(client_id).expect("client ids >= u16::MAX not supported for in-memory connections"));
            }
            ConnectionType::Native => self.native_count = self.native_count.saturating_add(1),
            ConnectionType::WasmWt => self.wasm_wt_count = self.wasm_wt_count.saturating_add(1),
            ConnectionType::WasmWs => self.wasm_ws_count = self.wasm_ws_count.saturating_add(1),
        }
    }

    /// The total number of clients.
    pub fn total(&self) -> usize {
        self.memory_clients
            .len()
            .saturating_add(self.native_count)
            .saturating_add(self.wasm_wt_count)
            .saturating_add(self.wasm_ws_count)
    }
}

//-------------------------------------------------------------------------------------------------------------------
