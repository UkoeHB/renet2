# CHANGELOG

## 0.12.1 - 01/03/26

- Replace `doc_auto_cfg` with `doc_cfg`.

## 0.12.0 - 01/02/26

- Update to `bevy_replicon` 0.37.

## 0.11.0 - 11/03/25

- Update to `bevy` 0.17 and `bevy_replicon` 0.36.

## 0.10.0 - 06/20/25

- Update to `bevy_replicon` 0.34.
    - Trigger disconnect when entities with `ConnectedClient` are despawned.

## 0.9.1 - 04/27/25

- Fix compile error with `bevy` Resource derives.

## 0.9.0 - 04/27/25

- Remove `/ws` path from websocket URLs.
- Update to `bevy` v0.16.

## 0.8.1 - 04/08/25

- Set upper bound of range of Firefox versions where webtransport was not fully supported (broken range: v133 - v135).

## 0.8.0 - 04/05/25

- Update `wtransport` to v0.6 in `renet2_netcode`.

## 0.7.0 - 03/24/25

- Update `bevy_replicon` to v0.32.

## 0.6.0 - 03/23/25

- Update `bevy_replicon` to v0.31.

## 0.5.0 - 02/21/25

- Add `WebSocketAcceptor::Plain::has_tls_proxy` field to fix setup bug when there is TLS via proxy.

## 0.4.0 - 02/19/25

- Fix panics in `renet2_setup::ClientCounts` methods.
- Update `GameServerSetupConfig` to include proxy IPs and allow proxy TLS for websockets.

## 0.3.0 - 02/07/25

- Replace `renet2_setup` crate's `ws_certs` feature with `ws-native-tls` and `ws-rustls` features.

## 0.2.0 - 02/06/25

- Add `renet2_setup` crate.
- Remove panics from `WebTransportServerConfig` API.
- Update `bevy_replicon` to v0.30.

## 0.1.2 - 02/03/25

- Allow non-`SocketAddr` urls in `WebSocketClientConfig`.

## 0.1.1 - 01/15/25

- Fix `webtransport_is_available_with_cert_hashes()` to detect if in a buggy Firefox version. See https://phabricator.services.mozilla.com/D231479

## 0.1.0 - 12/23/24

- Update `renet2` sub-crate dependencies.
    - `rustls`: 0.21 -> 0.23.5
    - `quinn`: 0.10 -> 0.11.6
    - `rcgen`: 0.12 -> 0.13
- Split `TransportSocket` into separate `ServerSocket`/`ClientSocket` traits.
- Add `webtransport_is_available()`/`webtransport_is_available_with_cert_hashes()` helpers for WASM clients.
- Add support for reliable transport sockets.
    - Add `TransportSocket::is_reliable`. It's true for in-memory sockets and WebSockets, and false for UDP and WebTransport.
    - Add `has_reliable_socket` argument to `RenetClient::new`
- Add WebSocket server and client. The client is WASM-only.
- Replace `h3` dependency with `wtransport` for WebTransport backend.

## 0.0.7 - 12/02/24

- Add section to README about building docs. Fixup doc links.
- Update `demo_bevy` workspace crate to bevy v0.14.
- Rename `ConnectionConfig::default()` to `ConnectionConfig::test()` and add constructor methods.
- Update to `bevy` v0.15.

## 0.0.6 - 09/22/24

- Remove `bevy_renet2` dependency on `bevy_window`.
- Properly clean up WebTransport client's reader stream.
- Update to `bevy_replicon` v0.28.1.
- Implement `Clone` for `MemorySocketClient` and `MemorySocketChannels`.
- Client ids for memory transports must now be manually defined. Note that in `bevy_replicon` client id `0` is reserved for listen servers.
- Update `bevy_replicon_renet2` to re-export the `client` and `server` features from `bevy_replicon`.

## 0.0.5 - 07/04/2024

- Update to Bevy v0.14.

## 0.0.4 - 06/26/2024

- Update `WebTransportClientConfig` to use `WebServerDestination`, which allows connecting to a WebTransport server via URL (useful when your server has certs for a domain name).
- Update `h3` dependencies for the WebTransport server in the `renet2` crate to depend on the `h3-v0.0.4` tag.
- Fix `disconnect_on_exit`. See [renet #158](https://github.com/lucaspoffo/renet/pull/158).
- Bump `bevy_replicon_renet2` to v0.0.4 for `bevy_replicon` v0.26.
- Loosen `cfg` on `webtransport_socket` module.

## 0.0.3 - 05/24/2024

- Add `bevy_replicon_renet2` sub-crate.
- Add `client_should_update` run condition to `bevy_renet2` to fix disconnect bug.

## 0.0.2 - 05/07/2024

- Fix WebTransport server panicking on construction when not inside a tokio runtime.

## 0.0.1 - 03/29/2024

- Forked from `renet`.
    - Implement `Reflect` on `ClientId`. See [renet #130](https://github.com/lucaspoffo/renet/pull/130).
    - Optimize `bevy_renet2` builds. See [renet #104](https://github.com/lucaspoffo/renet/pull/104).
    - Refactor RenetClient so channels are accessed more efficiently. See [renet #154](https://github.com/lucaspoffo/renet/pull/154).
    - Update `bevy_renet2` so client systems don't run when the client is disconnected. See [renet #134](https://github.com/lucaspoffo/renet/pull/134).
    - Add `TransportSocket` trait for injecting the source of unreliable packets to netcode transports. See [renet #145](https://github.com/lucaspoffo/renet/pull/145).
    - Add optional encryption to `renetcode2` to support sockets that handle encryption internally. See [renet #149](https://github.com/lucaspoffo/renet/pull/149).
    - Refactor `NetcodeServer` to allow multiple underlying sockets. See [renet #150](https://github.com/lucaspoffo/renet/pull/150).
    - Add memory-channels transport socket. See [renet #117](https://github.com/lucaspoffo/renet/pull/117).
    - Add WebTransport server and client implementations of TransportSocket. See [renet #107](https://github.com/lucaspoffo/renet/pull/107).
