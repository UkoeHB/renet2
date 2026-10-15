# Renetcode
[![Latest version](https://img.shields.io/crates/v/renetcode2.svg)](https://crates.io/crates/renetcode2)
[![Documentation](https://docs.rs/renetcode2/badge.svg)](https://docs.rs/renetcode2)
![MIT](https://img.shields.io/badge/license-MIT-blue.svg)
![Apache](https://img.shields.io/badge/license-Apache-blue.svg)


Renetcode2 is a simple connection based client/server protocol, was developed be used in games with UDP in mind, but can be used with other transports methods. Implements the Netcode 1.a2 standard with renet2 extensions, available [here][standard] and the original implementation in C is available in the [netcode][netcode] repository. See `NETCODE_EXTENSIONS.md` for the extensions implemented here.

Has the following feature:
- Encrypted and signed packets
- Secure client connection with connect tokens
- Connection based protocol

and protects the game server from the following attacks:
- Zombie clients
- Man in the middle
- DDoS amplification
- Packet replay attacks

[standard]: https://github.com/networkprotocol/netcode/blob/master/STANDARD.md
[netcode]: https://github.com/networkprotocol/netcode

## Usage
Check out the echo example to see an usage with UDP. Run the server with: 
```
cargo run --example echo_netcode -- server 5000 
```
run the client with:
```
cargo run --example echo_netcode -- client 5000 my_username
```
