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
            proxy_ip: None,
            wss_certs: None,
            ws_domain: None,
        }
    }

    #[cfg(feature = "ws_certs")]
    pub fn get_ws_acceptor(&self) -> Result<renet2_netcode::WebSocketAcceptor, String> {
        match Self::get_rustls_server_config(&self.wss_certs)? {
            Some(config) => Ok(renet2_netcode::WebSocketAcceptor::Rustls(config.into())),
            None => Ok(renet2_netcode::WebSocketAcceptor::Plain),
        }
    }

    /// Format: (cert chain, private key).
    /// Files must be PEM encoded.
    ///
    /// If there is no `rustls::crypto::CryptoProvider` installed, then the `ring` default provider will be
    /// auto-installed.
    #[cfg(feature = "ws_certs")]
    pub fn get_rustls_server_config(
        wss_certs: &Option<(PathBuf, PathBuf)>,
    ) -> Result<Option<std::sync::Arc<rustls::ServerConfig>>, String> {
        use rustls_pki_types::pem::PemObject;

        let Some((cert_chain, privkey)) = wss_certs else { return Ok(None) };

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
        Ok(Some(std::sync::Arc::new(config)))
    }
}

//-------------------------------------------------------------------------------------------------------------------
