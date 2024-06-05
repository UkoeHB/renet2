# CHANGELOG

## WIP

- Implement https://github.com/lucaspoffo/renet/pull/158.
- Bump `bevy_replicon_renet2` to v0.0.4 for `bevy_replicon` v0.26.
- Loosen `cfg` on `webtransport_socket` module.
- Optimize WebTransport client reader so datagrams are read directly into the packet buffer.

## 0.0.3 - 05/24/2024

- Add `bevy_replicon_renet2` sub-crate.
- Add `client_should_update` run condition to `bevy_renet2` to fix disconnect bug.

## 0.0.2 - 05/07/2024

- Fix WebTransport server panicking on construction when not inside a tokio runtime.

## 0.0.1 - 03/29/2024

- Forked from `renet`.
