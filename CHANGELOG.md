# CHANGELOG

## WIP

- Update `WebTransportClientConfig` to use `WebServerDestination`, which allows connecting to a WebTransport server via URL (useful when your server has certs for a domain name).
- Update `h3` dependencies for the WebTransport server in the `renet2` crate to depend on the `h3-v0.0.4` tag.
- Implement https://github.com/lucaspoffo/renet/pull/158.
- Bump `bevy_replicon_renet2` to v0.0.4 for `bevy_replicon` v0.26.
- Loosen `cfg` on `webtransport_socket` module.

## 0.0.3 - 05/24/2024

- Add `bevy_replicon_renet2` sub-crate.
- Add `client_should_update` run condition to `bevy_renet2` to fix disconnect bug.

## 0.0.2 - 05/07/2024

- Fix WebTransport server panicking on construction when not inside a tokio runtime.

## 0.0.1 - 03/29/2024

- Forked from `renet`.
