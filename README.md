# Renet2

![MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Apache](https://img.shields.io/badge/license-Apache-blue.svg)

*Fork of [renet](https://github.com/lucaspoffo/renet)*

Renet2 is a network library for Server/Client games written in rust. It is focused on fast-paced games such as FPS, and competitive games.

Provides the following features:

- Client/Server connection management
- Message based communication using channels, they can have different guarantees:
    - ReliableOrdered: guarantee of message delivery and order
    - ReliableUnordered: guarantee of message delivery but not order
    - Unreliable: no guarantee of message delivery or order
- Packet fragmention and reassembly
- Authentication and encryption, using [renetcode2](https://github.com/UkoeHB/renet2/tree/main/renetcode2)
    - The transport layer can be customizable. The default transport can be disabled and replaced with a custom one.
    - The underlying data transport/socket can be customized. Both unencrypted (e.g. UDP) and encrypted (e.g. WebTransport) data transports are supported.
- Built-in UDP sockets, in-memory sockets, WebTransport sockets, and WebSocket sockets for flexible, cross-platform networking using the `netcode` protocol.


## Differences from `renet`

### `netcode` protocol changes

Renet2 extends the original [netcode](https://github.com/networkprotocol/netcode) protocol with:

- Optional packet encryption. This supports data transports that do their own encryption.
- Optional transport reliability. This supports data transports that are automatically reliable.
- Servers with multiple concurrent data transports (e.g. UDP sockets and WebTransport).

See `renetcode2/NETCODE_EXTENSIONS.md`.

### Features

- Includes built-in data transports: UDP, memory channels, WebTransport, WebSockets.
    - See `src/examples` for a fully cross-platform demo.


## Building docs

Build workspace docs (no WASM):
```
cargo doc --open --no-deps --all-features
```

Build WASM docs (`renet2_netcode` workspace crate only):
```
cd renet2_netcode &&\
cargo doc --open --no-deps --no-default-features --features=wt_client_transport,ws_client_transport --target wasm32-unknown-unknown
```


## Channels

Renet communication is message based, and channels describe how the messages should be delivered.
Channels are unidirectional, `ConnectionConfig.client_channels_config` describes the channels that the clients sends to the server, and `ConnectionConfig.server_channels_config` describes the channels that the server sends to the clients.

Each channel has its own configuration `ChannelConfig`:

```rust
// No guarantee of message delivery or order
let send_type = SendType::Unreliable;
// Guarantee of message delivery and order
let send_type = SendType::ReliableOrdered {
    // If a message is lost, it will be resent after this duration
    resend_time: Duration::from_millis(300)
};

// Guarantee of message delivery but not order
let send_type = SendType::ReliableUnordered {
    resend_time: Duration::from_millis(300)
};

let channel_config = ChannelConfig {
    // The id for the channel, must be unique within its own list,
    // but it can be repeated between the server and client lists.
    channel_id: 0,
    // Maximum number of bytes that the channel may hold without acknowledgement of messages before becoming full.
    max_memory_usage_bytes: 5 * 1024 * 1024, // 5 mebibytes
    send_type
};
```

## Usage

Renet aims to have a simple API that is easy to integrate with any code base. Poll for new messages at the start of a frame with `update`. Call `send_packets` from the transport layer to send packets to the client/server.

### Server

```rust
let mut server = RenetServer::new(ConnectionConfig::default());

// Setup transport layer
const SERVER_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 5000);
let socket: UdpSocket = UdpSocket::bind(SERVER_ADDR).unwrap();
let server_config = ServerConfig {
    current_time: SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap(),
    max_clients: 64,
    protocol_id: 0,
    public_addresses: vec![SERVER_ADDR],
    authentication: ServerAuthentication::Unsecure
};
let mut transport = NetcodeServerTransport::new(server_config, socket).unwrap();

// Your gameplay loop
loop {
    let delta_time = Duration::from_millis(16);
    // Receive new messages and update clients
    server.update(delta_time);
    transport.update(delta_time, &mut server)?;
    
    // Check for client connections/disconnections
    while let Some(event) = server.get_event() {
        match event {
            ServerEvent::ClientConnected { client_id } => {
                println!("Client {client_id} connected");
            }
            ServerEvent::ClientDisconnected { client_id, reason } => {
                println!("Client {client_id} disconnected: {reason}");
            }
        }
    }

    // Receive message from channel
    for client_id in server.clients_id() {
        // The enum DefaultChannel describe the channels used by the default configuration
        while let Some(message) = server.receive_message(client_id, DefaultChannel::ReliableOrdered) {
            // Handle received message
        }
    }
    
    // Send a text message for all clients
    server.broadcast_message(DefaultChannel::ReliableOrdered, "server message");

    let client_id = 0;
    // Send a text message for all clients except for Client 0
    server.broadcast_message_except(client_id, DefaultChannel::ReliableOrdered, "server message");
    
    // Send message to only one client
    server.send_message(client_id, DefaultChannel::ReliableOrdered, "server message");
 
    // Send packets to clients using the transport layer
    transport.send_packets(&mut server);

    std::thread::sleep(delta_time); // Running at 60hz
}
```

### Client

```rust
let mut client = RenetClient::new(ConnectionConfig::default(), false);

// Setup transport layer
const server_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();
let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
let current_time = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
let authentication = ClientAuthentication::Unsecure {
    server_addr,
    client_id: 0,
    user_data: None,
    protocol_id: 0,
};

let mut transport = NetcodeClientTransport::new(current_time, authentication, socket).unwrap();

// Your gameplay loop
loop {
    let delta_time = Duration::from_millis(16);
    // Receive new messages and update client
    client.update(delta_time);
    transport.update(delta_time, &mut client).unwrap();
    
    if client.is_connected() {
        // Receive message from server
        while let Some(message) = client.receive_message(DefaultChannel::ReliableOrdered) {
            // Handle received message
        }
        
        // Send message
        client.send_message(DefaultChannel::ReliableOrdered, "client text");
    }
 
    // Send packets to server using the transport layer
    transport.send_packets(&mut client)?;
    
    std::thread::sleep(delta_time); // Running at 60hz
}
```

## Demos

You can checkout the [echo examples](https://github.com/UkoeHB/renet2/tree/main/examples/) for a simple usage of the library. Usage:

- Server: `cargo run --example echo -- server 5000`
- Client: `cargo run --example echo -- client 127.0.0.1:5000 CoolNickName`

Or you can look into the two demos that have more complex uses of renet:

<details><summary>Bevy Demo</summary>
<br/>
Simple bevy application to demonstrate how you could replicate entities and send reliable messages as commands from the server/client using renet:
<br/>
<br/>

[Bevy Demo.webm](https://user-images.githubusercontent.com/35241085/180664609-f8c969e0-d313-45c0-9c04-8a116896d0bd.webm)

[Repository](https://github.com/UkoeHb/renet2/tree/main/demo_bevy)
</details>

<details><summary>Chat Demo</summary>
<br/>
Simple chat application made with egui to demonstrate how you could handle errors, states transitions and client self hosting:
<br/>
<br/>

[Chat Demo.webm](https://user-images.githubusercontent.com/35241085/180664911-0baf7b35-c9d4-43ff-b793-5955060adebc.webm)

[Repository](https://github.com/UkoeHb/renet2/tree/main/demo_chat)
</details>

## Plugins

Check out [bevy_renet2](https://github.com/UkoeHb/renet2/tree/main/bevy_renet2) if you want to use renet2 as a plugin with the [Bevy engine](https://bevyengine.org/).

Check out [bevy_replicon_renet2](https://github.com/UkoeHb/renet2/tree/main/bevy_replicon_renet2) if you want to use renet2 as a backend for [bevy_replicon](https://github.com/projectharmonia/bevy_replicon).

Check out [renet2_steam](https://github.com/UkoeHb/renet2/tree/main/renet2_steam) if you want to use the steam transport layer instead of the default one.

## Visualizer

Check out [renet2_visualizer](https://github.com/UkoeHb/renet2/tree/main/renet2_visualizer) for an egui plugin to plot metrics data from renet clients and servers:

https://user-images.githubusercontent.com/35241085/175834010-b1eafd77-7ea2-47dc-a915-a399099c7a99.mp4

## Bevy compatibility

| bevy | renet2 |
|------|-------------------|
| 0.15 | [0.0.7](https://github.com/UkoeHB/renet2/releases/tag/0.0.7)         |
| 0.14 | [0.0.5](https://github.com/UkoeHB/renet2/releases/tag/0.0.5)         |
