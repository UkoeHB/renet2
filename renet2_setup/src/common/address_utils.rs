use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

//-------------------------------------------------------------------------------------------------------------------

/// Get an unspecified client address from a server address.
///
/// The type of the client address returned will be tailored to the type of the server address (Ipv4/Ipv6).
pub fn client_address_from_server_address(server_addr: &SocketAddr) -> SocketAddr {
    match server_addr {
        SocketAddr::V4(_) => SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0),
        SocketAddr::V6(_) => SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0),
    }
}

//-------------------------------------------------------------------------------------------------------------------
