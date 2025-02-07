Provides utilities for setting up `renet2` servers and clients.


## Server workflow

1. Define a `GameServerSetupConfig` for your server.
1. Collect the number of clients who will use each connection type into `ClientCounts`.
1. Make a `ConnectionConfig` with the channels for your renet2 connection with clients.
    - This should match the `ConnectionConfigs` used by your clients.
    - If using the `bevy_replicon_renet2` crate, then the channels can be obtained from `RepliconChannels`. Use `ConnectionConfigs::from_channels`.
1. Call `setup_combo_renet2_server` to get `RenetServe`, `NetcodeServerTransport`, and `ConnectMetas`.
    - If using the `bevy` feature, call `setup_combo_renet2_server_in_bevy` instead.
1. Drive the `RenetServer` and `NetcodeServerTransport` forward.
    - This is handled automatically if you use the `bevy_renet2` crate.
1. Use `ConnectMetas` to create `ServerConnectTokens` for clients based on their `ConnectionTypes`.
    - These 'metas' can be stored on a separate server from the game server.

### In-memory connections

Add in-memory clients to `ClientCounts` and follow the above steps.

### WebTransport certificates

This crate uses self-signed certificates to set up webtransport servers. Self-signed certificates only last 2 weeks, so if your game server lives longer than that and you need webtransport, then you should use the underlying renet2/renet2_netcode APIs instead of this crate.

Self-signed certificates are not supported everywhere. We assume clients will fall back to websockets if webtransport with self-signed certs are unavailable. `ConnectionType::inferred` will detect the correct connection type for each client.

### WebSocket TLS

Websocket TLS requires a domain name in `GameServerSetupConfig` and the locations of PEM-encoded cert files (e.g. generated with Let's Encrypt). You must specify the `ws-native-tls` or `ws-rustls` feature in addition to `ws_server_transport` in order to use websocket certs. Note that `ws-native-tls` requires OpenSSL, which may need to be installed separately on your server host.

If using `ws-rustls` and no `rustls::crypto::CryptoProvider` is installed, then `rustls::crypto::ring::default_provider().install_default()` will be called when setting up a websocket server.

### `tokio`

A default `tokio` runtime is set up if a server needs webtransport or websockets.


## Client workflow

1. Send your `ConnectionType` to the game backend.
    - Use `ConnectionType::inferred` to construct it.
1. Receive `ServerConnectToken` from the game backend.
1. Make a connect pack with `ClientConnectPack::new`.
1. Make a `ConnectionConfig` with the channels for your renet2 connection with the server.
    - This should match the `ConnectionConfig` used by the server.
    - If using the `bevy_replicon_renet2` crate, then the channels can be obtained from `RepliconChannels`. Use `ConnectionConfigs::from_channels`.
1. Call `setup_renet2_client` to get `RenetClient` and `NetcodeClientTransport`.
    - If using the `bevy` feature, call `setup_renet2_client_in_bevy` instead.
1. Drive the `RenetClient` and `NetcodeClientTransport` forward.
    - This is handled automatically if you use the `bevy_renet2` crate.

### In-memory connections

Receive `ServerConnectToken::Memory` from the local server (running in-memory with the client) and follow the above steps.
