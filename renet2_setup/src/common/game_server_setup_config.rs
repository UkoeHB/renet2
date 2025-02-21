use serde::{Deserialize, Serialize};

use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

//-------------------------------------------------------------------------------------------------------------------

/// Configuration details for setting up a renet2 server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameServerSetupConfig {
    /// Protocol id for server/client compatibility.
    pub protocol_id: u64,
    /// How long connect tokens should live before expiring.
    pub expire_secs: u64,
    /// Internal connection timeout for clients and servers.
    pub timeout_secs: i32,
    /// The server's IP address. Used for both native and WASM server sockets.
    ///
    /// This will be the *local* IP. To connect to the internet you likely need to set [`Self::proxy_ip`].
    pub server_ip: IpAddr,
    /// Port for native sockets.
    ///
    /// Set it to `0` if you don't need to target a specific port.
    pub native_port: u16,
    /// Port for webtransport sockets.
    ///
    /// Set it to `0` if you don't need to target a specific port.
    pub wasm_wt_port: u16,
    /// Port for websockets.
    ///
    /// Set it to `0` if you don't need to target a specific port.
    pub wasm_ws_port: u16,
    /// Public-facing port for native sockets.
    ///
    /// Set it to `0` to fall back to [`Self::native_port`].
    pub native_port_proxy: u16,
    /// Public-facing port for webtransport sockets.
    ///
    /// Set it to `0` to fall back to [`Self::wasm_wt_port`].
    pub wasm_wt_port_proxy: u16,
    /// Public-facing port for websockets.
    ///
    /// Set it to `0` to fall back to [`Self::wasm_ws_port`].
    pub wasm_ws_port_proxy: u16,
    /// Proxy IP address to send to clients in connect tokens instead of the `server_ip`.
    ///
    /// Proxy IP addresses will be associated with the local ports assigned to each socket.
    pub proxy_ip: Option<IpAddr>,
    /// Domain name to use instead of the proxy_ip for websocket servers.
    ///
    /// This is required if using [`Self::wss_certs`].
    pub ws_domain: Option<String>,
    /// Location of certificate files to use for websocket servers.
    ///
    /// Format: (cert chain, private key).
    /// Files must be PEM encoded.
    pub wss_certs: Option<(PathBuf, PathBuf)>,
    /// Indicates if there is a TLS proxy set up for websocket connections.
    ///
    /// If this is true then [`Self::wss_certs`] should be `None`.
    pub has_wss_proxy: bool,
}

impl GameServerSetupConfig {
    /// Make a dummy config.
    ///
    /// Should not be used to connect to a real renet server.
    pub fn dummy() -> Self {
        Self {
            protocol_id: 0u64,
            expire_secs: 10u64,
            timeout_secs: 5i32,
            server_ip: Ipv4Addr::LOCALHOST.into(),
            native_port: 0,
            wasm_wt_port: 0,
            wasm_ws_port: 0,
            native_port_proxy: 0,
            wasm_wt_port_proxy: 0,
            wasm_ws_port_proxy: 0,
            proxy_ip: None,
            wss_certs: None,
            ws_domain: None,
            has_wss_proxy: false,
        }
    }

    #[cfg(feature = "ws_server_transport")]
    pub fn get_ws_acceptor(&self) -> Result<renet2_netcode::WebSocketAcceptor, String> {
        let Some((cert_chain, privkey)) = &self.wss_certs else {
            return Ok(renet2_netcode::WebSocketAcceptor::Plain {
                has_tls_proxy: self.has_wss_proxy,
            });
        };

        #[cfg(feature = "ws-native-tls")]
        {
            let config = Self::get_native_tls_acceptor(cert_chain, privkey)?;
            return Ok(renet2_netcode::WebSocketAcceptor::NativeTls(config.into()));
        }

        #[cfg(feature = "ws-rustls")]
        {
            let config = Self::get_rustls_server_config(cert_chain, privkey)?;
            return Ok(renet2_netcode::WebSocketAcceptor::Rustls(config.into()));
        }

        #[cfg(not(any(feature = "ws-native-tls", feature = "ws-rustls")))]
        {
            Err(format!(
                "failed getting websocket acceptor for certs {cert_chain:?} and {privkey:?}; missing feature ws-native-tls or \
                ws-rustls"
            ))
        }
    }

    /// Format: (cert chain, private key).
    /// Files must be PEM encoded. The certs must be x509 and the privkey must be PKCS #8.
    #[cfg(feature = "ws-native-tls")]
    pub fn get_native_tls_acceptor(cert_chain: &PathBuf, privkey: &PathBuf) -> Result<tokio_native_tls::native_tls::TlsAcceptor, String> {
        let certs = std::fs::read(cert_chain)
            .map_err(|err| format!("failed reading cert chain at {cert_chain:?} for native tls acceptor: {err:?}"))?;
        let privkey =
            std::fs::read(privkey).map_err(|err| format!("failed reading privkey at {privkey:?} for native tls acceptor: {err:?}"))?;
        let identity = tokio_native_tls::native_tls::Identity::from_pkcs8(&certs, &privkey)
            .map_err(|err| format!("failed constructing native tls Identity: {err:?}"))?;
        tokio_native_tls::native_tls::TlsAcceptor::new(identity)
            .map_err(|err| format!("failed constructing native tls TlsAcceptor: {err:?}"))
    }

    /// Format: (cert chain, private key).
    /// Files must be PEM encoded.
    ///
    /// If there is no `rustls::crypto::CryptoProvider` installed, then the `ring` default provider will be
    /// auto-installed.
    #[cfg(feature = "ws-rustls")]
    pub fn get_rustls_server_config(cert_chain: &PathBuf, privkey: &PathBuf) -> Result<std::sync::Arc<rustls::ServerConfig>, String> {
        use rustls_pki_types::pem::PemObject;

        let mut file_iter = rustls_pki_types::CertificateDer::pem_file_iter(cert_chain)
            .map_err(|err| format!("failed reading {cert_chain:?} for websocket certs: {err:?}"))?;
        let mut certs = Vec::default();
        file_iter.try_for_each(|i| {
            let cert = i.map_err(|err| format!("failure while reading {cert_chain:?} for websocket certs: {err:?}"))?;
            certs.push(cert);
            Ok::<(), String>(())
        })?;
        let privkey = rustls_pki_types::PrivateKeyDer::from_pem_file(privkey)
            .map_err(|err| format!("failed reading {privkey:?} for websocket certs privkey: {err:?}"))?;
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, privkey)
            .map_err(|err| format!("failed building rustls serverconfig with websocket certs: {err:?}"))?;
        Ok(std::sync::Arc::new(config))
    }
}

//-------------------------------------------------------------------------------------------------------------------
