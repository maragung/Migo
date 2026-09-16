//! The TLS 1.3 channel every server-to-server mesh link rides in (section 7).
//!
//! The mesh's own security — the Ed25519 handshake, the replay window, the allow-list —
//! lives at the application layer in [`migo_federation`](crate::mesh), and section 7 is
//! explicit about why it stays there: a TLS client certificate alone is not enough,
//! because the mesh identity lives at the application layer so it never depends on an
//! external PKI. This module is the other half the same section mandates, the half that
//! has been missing: every link is encrypted and integrity-protected, TLS 1.3 over TCP,
//! with no downgrade path and no plaintext mode — not for development, not for staging.
//!
//! # Certificates are carriers, not identities
//!
//! Each node mints one self-signed X.509 leaf from its Ed25519 node identity key at
//! startup, with [`rcgen`]. The certificate carries no PKI meaning: no CA signed it, no
//! chain is checked, no hostname is matched, no expiry window is consulted. It exists
//! because a TLS 1.3 handshake needs a certificate structure to move a public key, and
//! the key it moves is the node's mesh identity key — so the allow-list entry a peer
//! already holds, the 32-byte Ed25519 public key, is the whole of the pin. Both
//! verifiers below accept exactly the leaf whose subject public key equals an allow-list
//! key and reject everything else, because the identity decision belongs to the
//! application handshake that runs immediately after the channel is up.
//!
//! A peer that is not in the allow-list is refused here, at the TLS handshake, before a
//! single mesh byte is exchanged — section 7's refusal at the handshake, before a session
//! forms — and a plaintext client cannot complete the handshake at all, so MWP frames
//! never ride a bare socket.
//!
//! # What rides inside
//!
//! Nothing on the wire changes: the length-prefixed MWP/1 frames [`crate::mesh`] reads
//! and writes today are handed to the TLS stream unmodified, so no frame byte, schema, or
//! generated struct moves. TLS 1.3 only — the configs are built with
//! [`rustls::version::TLS13`] as the single supported version, so a peer offering 1.2 is
//! refused rather than downgraded.

use std::sync::Arc;

use migo_core::Result;
use migo_crypto::NodeSecret;
use migo_protocol::fault;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, SubjectPublicKeyInfoDer,
};

/// The DER prefix of a PKCS#8 v1 Ed25519 private key: the sixteen bytes in front of the
/// 32-byte seed. Splicing it in front of the node identity seed is the whole of turning a
/// [`NodeSecret`] into the PKCS#8 structure `rcgen` and `rustls` consume.
const ED25519_PKCS8_PREFIX: &[u8] = &[
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// The DER SubjectPublicKeyInfo an Ed25519 certificate carries for `key`: a fixed
/// twelve-byte algorithm header in front of the 32 raw key bytes. Building it locally —
/// rather than parsing the peer's SPKI into pieces — makes the pin a whole-structure
/// byte comparison: the peer must present exactly this SPKI, not merely something that
/// happens to contain the key somewhere inside.
const ED25519_SPKI_PREFIX: &[u8] = &[
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The DER SubjectPublicKeyInfo an Ed25519 leaf carries for `key`.
fn ed25519_spki(key: &[u8; 32]) -> SubjectPublicKeyInfoDer<'static> {
    let mut der = Vec::with_capacity(ED25519_SPKI_PREFIX.len() + key.len());
    der.extend_from_slice(ED25519_SPKI_PREFIX);
    der.extend_from_slice(key);
    SubjectPublicKeyInfoDer::from(der)
}

/// Reads a presented leaf's subject public key.
///
/// The parse is [`rustls_webpki`]'s — the same X.509 reader rustls hands every
/// certificate to — so a leaf too malformed to read here is too malformed to have a
/// place in a mesh handshake at all.
fn leaf_spki(end_entity: &CertificateDer<'_>) -> Result<SubjectPublicKeyInfoDer<'static>> {
    let owned = end_entity.to_owned();
    let cert = rustls_webpki::EndEntityCert::try_from(&owned)
        .map_err(|_| fault::internal("the peer's mesh certificate is not a readable X.509 leaf"))?;
    Ok(cert.subject_public_key_info())
}

/// The node's TLS identity for mesh links: one self-signed leaf whose key is the node's
/// Ed25519 mesh identity, minted once at startup and held for the process's life.
pub struct MeshTls {
    /// The minted leaf, presented on both sides of every handshake this node joins.
    certificate: CertificateDer<'static>,
    /// The PKCS#8 form of the node identity seed, for rustls's key provider.
    private_key: PrivatePkcs8KeyDer<'static>,
    /// The ring provider every config below is built on, held so the verifiers and the
    /// builders agree on one provider instead of racing for the process default.
    provider: Arc<CryptoProvider>,
}

impl MeshTls {
    /// Mints the node's TLS leaf from its mesh identity key.
    ///
    /// [`NodeSecret::expose_seed`] is otherwise reserved for `migod keygen`; it is used
    /// here because the certificate must carry *this* node's identity key — minting a
    /// fresh TLS key would move the pin off the allow-list key the peer already holds,
    /// which is the one thing this channel must not do. The copy of the seed inside the
    /// PKCS#8 structure is the cost of handing the key to the TLS layer, the same cost
    /// any server pays to hold its private key for the process's life.
    ///
    /// # Errors
    ///
    /// If the key cannot be loaded into the TLS layer, the leaf cannot be minted, or the
    /// minted leaf does not carry the identity key — all boot failures, because section 7
    /// gives the mesh no plaintext mode to fall back to.
    pub fn from_secret(secret: &NodeSecret) -> Result<Self> {
        let mut pkcs8 = Vec::with_capacity(ED25519_PKCS8_PREFIX.len() + 32);
        pkcs8.extend_from_slice(ED25519_PKCS8_PREFIX);
        pkcs8.extend_from_slice(&secret.expose_seed());
        let key_pair = rcgen::KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(pkcs8.clone()),
            &rcgen::PKCS_ED25519,
        )
        .map_err(|error| {
            fault::internal(format!(
                "cannot load the node key into the TLS layer: {error}"
            ))
        })?;
        // The certificate's subject is the key's fingerprint, so an operator reading a
        // handshake with `openssl s_client` sees which node key is speaking. No hostname
        // goes in, because no verifier below reads one — identity is the key, not a name.
        let mut params = rcgen::CertificateParams::default();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, secret.public().fingerprint());
        let certificate = params.self_signed(&key_pair).map_err(|error| {
            fault::internal(format!("cannot mint the mesh TLS certificate: {error}"))
        })?;
        let tls = Self {
            certificate: certificate.der().clone(),
            private_key: PrivatePkcs8KeyDer::from(pkcs8),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        };
        // The leaf must carry exactly the identity key the allow-list pins. It cannot
        // fail mathematically — the leaf was just minted from the seed — but the check
        // makes the contract loud: a mismatched leaf would speak for another node's key,
        // so it is refused rather than served.
        let minted = leaf_spki(&tls.certificate)?;
        if minted.as_ref() != ed25519_spki(&secret.public().to_bytes()).as_ref() {
            return Err(fault::internal(
                "the minted mesh TLS certificate does not carry the node identity key",
            ));
        }
        Ok(tls)
    }

    /// The TLS 1.3 server config for one inbound connection, pinning `allowed` — every
    /// key in this node's allow-list — as the set of client leaves it will accept.
    ///
    /// Built per connection because the allow-list moves: a peer admitted a minute ago
    /// is welcome on its next dial without a restart, and a peer removed is refused on
    /// it. The handshake itself requires the client's leaf; a client that presents none,
    /// or one whose key is not in the set, is refused before any mesh byte is read.
    ///
    /// # Errors
    ///
    /// If the config cannot be shaped or the leaf cannot be loaded — a boot-time defect,
    /// not a peer condition.
    pub fn server_config(&self, allowed: &[[u8; 32]]) -> Result<Arc<rustls::ServerConfig>> {
        let verifier = AllowListClientCert {
            expected: allowed.iter().map(|key| ed25519_spki(key)).collect(),
            provider: Arc::clone(&self.provider),
        };
        let config = rustls::ServerConfig::builder_with_provider(Arc::clone(&self.provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|error| fault::internal(format!("cannot shape the mesh TLS config: {error}")))?
            .with_client_cert_verifier(Arc::new(verifier))
            .with_single_cert(
                vec![self.certificate.clone()],
                PrivateKeyDer::Pkcs8(self.private_key.clone()),
            )
            .map_err(|error| fault::internal(format!("cannot load the mesh TLS leaf: {error}")))?;
        Ok(Arc::new(config))
    }

    /// The TLS 1.3 client config for dialing one peer, pinned to `expected` — the key
    /// this node's allow-list entry names for that peer. The dialer presents its own
    /// leaf, so the peer's gate sees this node's identity key in return.
    ///
    /// # Errors
    ///
    /// If the config cannot be shaped or the leaf cannot be loaded — a boot-time defect,
    /// not a peer condition.
    pub fn client_config(&self, expected: [u8; 32]) -> Result<Arc<rustls::ClientConfig>> {
        let verifier = PinnedServerCert {
            expected: ed25519_spki(&expected),
            provider: Arc::clone(&self.provider),
        };
        let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&self.provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|error| fault::internal(format!("cannot shape the mesh TLS config: {error}")))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_client_auth_cert(
                vec![self.certificate.clone()],
                PrivateKeyDer::Pkcs8(self.private_key.clone()),
            )
            .map_err(|error| fault::internal(format!("cannot load the mesh TLS leaf: {error}")))?;
        Ok(Arc::new(config))
    }
}

/// The client half of the pin: accepts exactly the server leaf whose key is `expected`.
///
/// Chain, hostname, and expiry are ignored by design — the leaf is a carrier for the
/// allow-listed key, and the identity decision belongs to the application handshake that
/// runs next. Anything else the leaf might claim about its signer is decoration this
/// verifier refuses to trust.
#[derive(Debug)]
struct PinnedServerCert {
    /// The one SPKI a server may present: the dialed peer's allow-list key.
    expected: SubjectPublicKeyInfoDer<'static>,
    /// The provider the handshake signature check delegates to.
    provider: Arc<CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let presented = leaf_spki(end_entity).map_err(|_| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
        })?;
        if presented.as_ref() == self.expected.as_ref() {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the peer's certificate does not carry the key this node's allow-list names for it"
                    .to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // TLS 1.2 is never offered on a mesh link: the config above speaks 1.3 only, so a
        // 1.2 signature here would mean the pin was bypassed, and the only answer is a
        // refusal.
        Err(rustls::Error::General(
            "a mesh link speaks TLS 1.3 only; a TLS 1.2 signature has no business appearing"
                .to_owned(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// The server half of the pin: accepts exactly the client leaf whose key is one of
/// `expected` — the keys in this node's allow-list at the moment the connection arrived.
///
/// Membership is by key alone, whatever the peer's status row says: pausing and blocking
/// are runtime policy the application handshake already refuses, and the TLS gate's one
/// job is section 7's — a node whose key the operator never admitted does not get to
/// finish a handshake.
#[derive(Debug)]
struct AllowListClientCert {
    /// Every SPKI a client may present: one per allow-list entry.
    expected: Vec<SubjectPublicKeyInfoDer<'static>>,
    /// The provider the handshake signature check delegates to.
    provider: Arc<CryptoProvider>,
}

impl rustls::server::danger::ClientCertVerifier for AllowListClientCert {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        // There is no CA to hint at: the pin is the key, not a chain, so the client is
        // told nothing about what to present — it presents its node leaf or nothing.
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        let presented = leaf_spki(end_entity).map_err(|_| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
        })?;
        if self
            .expected
            .iter()
            .any(|key| presented.as_ref() == key.as_ref())
        {
            Ok(rustls::server::danger::ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the dialer's certificate does not carry a key in this node's allow-list"
                    .to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::server::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General(
            "a mesh link speaks TLS 1.3 only; a TLS 1.2 signature has no business appearing"
                .to_owned(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::server::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node key from a fixed seed, the way every mesh test builds one.
    fn secret(seed: u8) -> NodeSecret {
        NodeSecret::from_seed(&[seed; 32]).expect("a 32-byte seed builds a key")
    }

    #[test]
    fn the_minted_leaf_carries_the_node_identity_key() {
        for seed in [1u8, 2, 0xff] {
            let node = secret(seed);
            let tls = MeshTls::from_secret(&node).expect("the identity key mints a leaf");
            let spki = leaf_spki(&tls.certificate).expect("the minted leaf parses");
            assert_eq!(
                spki.as_ref(),
                ed25519_spki(&node.public().to_bytes()).as_ref(),
                "the leaf's public key must be the node identity key, seed {seed}"
            );
        }
    }

    #[test]
    fn the_spki_pin_is_the_full_der_structure() {
        let node = secret(3);
        let pinned = ed25519_spki(&node.public().to_bytes());
        // The full DER: the SEQUENCE wrapper, the Ed25519 algorithm id, and the 32 key
        // bytes — 44 in total. Anything shorter or longer is not the pin.
        assert_eq!(pinned.as_ref().len(), 44);
        assert_ne!(
            pinned.as_ref(),
            node.public().to_bytes().as_slice(),
            "the pin is the wrapped SPKI, not the bare key bytes"
        );
    }
}
