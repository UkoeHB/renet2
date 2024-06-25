use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

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

/// Serializable version of [`WebServerDestination`].
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WebServerDestinationSerializable {
    Addr(String),
    Url(String),
}

impl From<WebServerDestination> for WebServerDestinationSerializable {
    fn from(dest: WebServerDestination) -> Self {
        match dest {
            WebServerDestination::Addr(addr) => Self::Addr(addr.to_string()),
            WebServerDestination::Url(url) => Self::Addr(url.to_string()),
        }
    }
}

impl TryFrom<WebServerDestinationSerializable> for WebServerDestination {
    type Error = ();
    fn try_from(dest: WebServerDestinationSerializable) -> Result<Self, Self::Error> {
        match dest {
            WebServerDestinationSerializable::Addr(addr) => Ok(Self::Addr(addr.parse().map_err(|_| ())?)),
            WebServerDestinationSerializable::Url(url) => Ok(Self::Url(url.parse().map_err(|_| ())?)),
        }
    }
}

/// Key for `netcode` connection requests inserted as query pairs into `WebTransport` connection requests.
#[cfg(any(feature = "wt_server_transport", feature = "wt_client_transport"))]
pub(crate) const WT_CONNECT_REQ: &str = "creq";

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
