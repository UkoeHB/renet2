use std::net::SocketAddr;

use super::bindings::WebTransport;
use crate::{ServerCertHash, WebTransportClientConfig};

/// Checks if the current WASM operating environment supports WebTransport.
///
/// See [`webtransport_is_available_with_cert_hashes`] if you want to use
/// [`serverCertificateHashes`](WebTransportClientConfig::server_cert_hashes).
pub fn webtransport_is_available() -> bool {
    webtransport_is_available_impl(false)
}

/// Checks if the current WASM operating environment supports WebTransport with
/// [`serverCertificateHashes`](WebTransportClientConfig::server_cert_hashes).
///
/// See [`webtransport_is_available`] if you don't care about cert hashes.
pub fn webtransport_is_available_with_cert_hashes() -> bool {
    !buggy_firefox_version() && webtransport_is_available_impl(true)
}

fn webtransport_is_available_impl(with_cert_hashes: bool) -> bool {
    let mock_addr: SocketAddr = "127.0.0.1:4433".parse().unwrap();
    let mut cert_hashes = vec![];
    if with_cert_hashes {
        cert_hashes.push(ServerCertHash { hash: [0; 32] });
    }
    let config = WebTransportClientConfig::new_with_certs(mock_addr, cert_hashes);
    let url: url::Url = config.server_dest.clone().try_into().unwrap();

    // Errors when WebTransport isn't available or when `config` is not supported.
    // - https://developer.mozilla.org/en-US/docs/Web/API/WebTransport/WebTransport#exceptions
    WebTransport::new_with_options(url.as_str(), &config.wt_options()).is_ok()
}

// Note: this test can fail if the user modified their firefox user-agent string from the default.
fn buggy_firefox_version() -> bool {
    // Firefox workaround for bug in v133-v136.
    // TODO: update this to filter on the correct version range when the bug is fixed
    if let Some(window) = web_sys::window() {
        if let Ok(user_agent_str) = window.navigator().user_agent() {
            if let Some((_, firefox)) = user_agent_str.split_once("Firefox/") {
                if let Some(version) = firefox.get(0..=4) {
                    if let Ok(version) = version.parse::<f32>() {
                        if version >= 133.0 && version < 136.0 {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}
