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

/// Key for `netcode` connection requests inserted as query pairs into `WebTransport` connection requests.
#[cfg(any(feature = "wt_server_transport", feature = "wt_client_transport"))]
pub(crate) const WT_CONNECT_REQ: &str = "creq";
