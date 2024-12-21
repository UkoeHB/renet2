use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6};

/// SHA-256 hash of a DER-encoded certificate.
///
/// The certificate must be an X.509v3 certificate that has a validity period of less that 2 weeks, and the
/// current time must be within that validity period. The format of the public key in the certificate depends
/// on the implementation, but must minimally include ECDSA with the secp256r1 (NIST P-256) named group, and
/// must not include RSA keys.
/// See the [docs](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#servercertificatehashes).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ServerCertHash {
    pub hash: [u8; 32],
}

impl TryFrom<Vec<u8>> for ServerCertHash {
    type Error = ();

    fn try_from(vec: Vec<u8>) -> Result<Self, ()> {
        if vec.len() != 32 {
            return Err(());
        }
        let mut hash = [0; 32];
        hash.copy_from_slice(&vec[0..32]);
        Ok(Self { hash })
    }
}

/// Represents a WebTransport server destination.
///
/// When setting up WebTransport servers and clients, this destination must be used.
/// - `WebTransportClientConfig::server_dest`: Set the destination here. This tells the client where to connect.
/// - `ServerSetupConfig::server_addresses`: Store the destination as a `SocketAddr` in the server addresses for the
///   WebTransport server. This is used to validate connect tokens.
/// - `ClientAuthentication`: Use the destination in `ClientAuthentication::Unsecure::server_address` and in
///   `ConnectToken::generate()` for secure auth. The server address is used internally by `renet2` to coordinate
///   packet sending and receiving.
///
/// The conversion `SocketAddr::from(WebServerDestination::Url)` is *not* reversible. The conversion involves
/// hashing the URL and pasting it into the socket address bytes (28 bytes total).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WebServerDestination {
    Addr(SocketAddr),
    /// The server destination as a URL.
    ///
    /// This URL [must include](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport) the
    /// port number explicitly.
    Url(url::Url),
}

impl From<SocketAddr> for WebServerDestination {
    fn from(addr: SocketAddr) -> Self {
        Self::Addr(addr)
    }
}

impl From<url::Url> for WebServerDestination {
    fn from(url: url::Url) -> Self {
        Self::Url(url)
    }
}

impl From<WebServerDestination> for SocketAddr {
    fn from(dest: WebServerDestination) -> Self {
        match dest {
            WebServerDestination::Addr(addr) => addr,
            WebServerDestination::Url(url) => hash_url_to_socket_addr(url),
        }
    }
}

impl TryFrom<WebServerDestination> for url::Url {
    type Error = ();
    fn try_from(dest: WebServerDestination) -> Result<Self, Self::Error> {
        match dest {
            WebServerDestination::Addr(addr) => wt_server_addr_to_url(addr),
            WebServerDestination::Url(url) => Ok(url),
        }
    }
}

/// Key for `netcode` connection requests inserted as query pairs into HTTP connection requests.
#[allow(unused)]
pub(crate) const HTTP_CONNECT_REQ: &str = "creq";

fn hash_url_to_socket_addr(url: url::Url) -> SocketAddr {
    let hash = hmac_sha256::Hash::hash(url.as_str().as_bytes());

    SocketAddrV6::new(
        Ipv6Addr::from(TryInto::<[u8; 16]>::try_into(&hash[0..16]).unwrap()),
        u16::from_le_bytes(hash[16..18].try_into().unwrap()),
        u32::from_le_bytes(hash[18..22].try_into().unwrap()),
        u32::from_le_bytes(hash[22..26].try_into().unwrap()),
    )
    .into()
}

fn wt_server_addr_to_url(addr: SocketAddr) -> Result<url::Url, ()> {
    let mut url = url::Url::parse("https://example.net").map_err(|_| ())?;
    url.set_ip_host(addr.ip())?;
    url.set_port(Some(addr.port()))?;
    Ok(url)
}

#[allow(unused)]
pub(crate) fn client_idx_to_addr(idx: u64) -> SocketAddr {
    SocketAddr::new(
        IpAddr::V6(Ipv6Addr::new(
            idx as u16,
            (idx >> 16) as u16,
            (idx >> 32) as u16,
            (idx >> 48) as u16,
            0,
            0,
            0,
            0,
        )),
        0,
    )
}

#[allow(unused)]
pub(crate) fn client_idx_from_addr(addr: SocketAddr) -> u64 {
    let SocketAddr::V6(addr_v6) = addr else {
        panic!("V6 addresses are expected to represent client idxs")
    };
    let octets = addr_v6.ip().octets();

    let mut idx = 0u64;
    for i in (0..4).rev() {
        idx <<= 16;
        idx += ((octets[2 * i] as u64) << 8) + (octets[2 * i + 1] as u64);
    }

    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_addr_conversion() {
        let addr0 = client_idx_to_addr(0);
        let addr1 = client_idx_to_addr(1);
        let addr16 = client_idx_to_addr(16);
        let addr257 = client_idx_to_addr(257);

        assert_eq!(client_idx_from_addr(addr0), 0);
        assert_eq!(client_idx_from_addr(addr1), 1);
        assert_eq!(client_idx_from_addr(addr16), 16);
        assert_eq!(client_idx_from_addr(addr257), 257);
    }
}
