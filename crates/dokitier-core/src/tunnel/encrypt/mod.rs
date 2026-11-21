use crate::{config::EncryptionAlgorithm, packet::ZCPacket};
use std::sync::Arc;

#[cfg(feature = "aes-gcm")]
#[cfg_attr(
    any(feature = "openssl-crypto", feature = "ring-crypto"),
    allow(dead_code)
)]
pub mod aes_gcm;
#[cfg(feature = "chacha20")]
#[cfg_attr(
    any(feature = "openssl-crypto", feature = "ring-crypto"),
    allow(dead_code)
)]
pub mod chacha20;
#[cfg(feature = "openssl-crypto")]
mod openssl;
#[cfg(all(feature = "ring-crypto", any(not(feature = "openssl-crypto"), test)))]
mod ring;
#[cfg(all(target_os = "wasi", feature = "wasi-crypto-offload"))]
mod wasi_host;

pub mod xor;

// The disabled backends keep the same error Interface as the AEAD backends.
#[allow(dead_code)]
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("packet is too short. len: {0}")]
    PacketTooShort(usize),
    #[error("decryption failed")]
    DecryptionFailed,
    #[error("encryption failed")]
    EncryptionFailed,
    #[error("invalid encryption algorithm: {0}")]
    InvalidAlgorithm(String),
    #[error("encryption algorithm is unavailable in this build: {0}")]
    AlgorithmUnavailable(String),
}

pub trait Encryptor: Send + Sync + 'static {
    fn decrypt(&self, zc_packet: &mut ZCPacket) -> Result<(), Error>;
    fn encrypt(&self, zc_packet: &mut ZCPacket) -> Result<(), Error>;
    fn encrypt_with_nonce(
        &self,
        zc_packet: &mut ZCPacket,
        _nonce: Option<&[u8]>,
    ) -> Result<(), Error> {
        self.encrypt(zc_packet)
    }
}

pub struct NullCipher;

struct UnsupportedCipher {
    algorithm: String,
    unavailable: bool,
}

/// Domain-separated SHA-256 key derivation.
///
/// The previous implementation used `std::collections::hash_map::DefaultHasher`
/// (SipHash-1-3). Its output is an implementation detail of the standard
/// library: it is not a stable contract across Rust releases, so two builds of
/// the same node could derive different keys from the same network secret —
/// a fleet-wide silent decryption failure after a toolchain bump. SHA-256 is a
/// fixed, well-defined function and is safe to depend on.
fn derive_key(secret: &str, domain: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"dokitier-network-secret-v1");
    hasher.update((domain.len() as u32).to_be_bytes());
    hasher.update(domain);
    hasher.update((secret.len() as u64).to_be_bytes());
    hasher.update(secret.as_bytes());
    hasher.finalize().into()
}

pub fn derive_key_128(secret: &str) -> [u8; 16] {
    let digest = derive_key(secret, b"aes128");
    let mut key = [0u8; 16];
    key.copy_from_slice(&digest[..16]);
    key
}

pub fn derive_key_256(secret: &str) -> [u8; 32] {
    derive_key(secret, b"aes256")
}

impl Encryptor for NullCipher {
    fn decrypt(&self, zc_packet: &mut ZCPacket) -> Result<(), Error> {
        let pm_header = zc_packet.peer_manager_header().unwrap();
        if pm_header.is_encrypted() {
            Err(Error::DecryptionFailed)
        } else {
            Ok(())
        }
    }

    fn encrypt(&self, _zc_packet: &mut ZCPacket) -> Result<(), Error> {
        Ok(())
    }
}

impl UnsupportedCipher {
    fn error(&self) -> Error {
        if self.unavailable {
            Error::AlgorithmUnavailable(self.algorithm.clone())
        } else {
            Error::InvalidAlgorithm(self.algorithm.clone())
        }
    }
}

impl Encryptor for UnsupportedCipher {
    fn decrypt(&self, _zc_packet: &mut ZCPacket) -> Result<(), Error> {
        Err(self.error())
    }

    fn encrypt(&self, _zc_packet: &mut ZCPacket) -> Result<(), Error> {
        Err(self.error())
    }
}

fn invalid_encryptor(algorithm: &str) -> Arc<dyn Encryptor> {
    Arc::new(UnsupportedCipher {
        algorithm: algorithm.to_owned(),
        unavailable: false,
    })
}

#[allow(dead_code)] // Selected disabled backends call this in reduced profiles.
fn unavailable_encryptor(algorithm: &str) -> Arc<dyn Encryptor> {
    Arc::new(UnsupportedCipher {
        algorithm: algorithm.to_owned(),
        unavailable: true,
    })
}

pub(crate) fn algorithm_is_available(algorithm: EncryptionAlgorithm) -> bool {
    match algorithm {
        EncryptionAlgorithm::Xor => true,
        EncryptionAlgorithm::AesGcm | EncryptionAlgorithm::Aes256Gcm => cfg!(any(
            feature = "aes-gcm",
            feature = "openssl-crypto",
            feature = "ring-crypto"
        )),
        EncryptionAlgorithm::ChaCha20 => cfg!(any(
            feature = "chacha20",
            feature = "openssl-crypto",
            feature = "ring-crypto"
        )),
    }
}

fn is_aead_algorithm(algorithm: EncryptionAlgorithm) -> bool {
    matches!(
        algorithm,
        EncryptionAlgorithm::AesGcm
            | EncryptionAlgorithm::Aes256Gcm
            | EncryptionAlgorithm::ChaCha20
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AeadBackend {
    #[cfg(feature = "openssl-crypto")]
    OpenSsl,
    #[cfg(all(not(feature = "openssl-crypto"), feature = "ring-crypto"))]
    Ring,
    #[cfg(all(
        not(feature = "openssl-crypto"),
        not(feature = "ring-crypto"),
        any(feature = "aes-gcm", feature = "chacha20")
    ))]
    RustCrypto,
}

#[cfg(feature = "openssl-crypto")]
fn preferred_aead_backend(algorithm: EncryptionAlgorithm) -> Option<AeadBackend> {
    is_aead_algorithm(algorithm).then_some(AeadBackend::OpenSsl)
}

#[cfg(all(not(feature = "openssl-crypto"), feature = "ring-crypto"))]
fn preferred_aead_backend(algorithm: EncryptionAlgorithm) -> Option<AeadBackend> {
    is_aead_algorithm(algorithm).then_some(AeadBackend::Ring)
}

#[cfg(all(
    not(feature = "openssl-crypto"),
    not(feature = "ring-crypto"),
    any(feature = "aes-gcm", feature = "chacha20")
))]
fn preferred_aead_backend(algorithm: EncryptionAlgorithm) -> Option<AeadBackend> {
    (is_aead_algorithm(algorithm) && algorithm_is_available(algorithm))
        .then_some(AeadBackend::RustCrypto)
}

#[cfg(not(any(
    feature = "openssl-crypto",
    feature = "ring-crypto",
    feature = "aes-gcm",
    feature = "chacha20"
)))]
fn preferred_aead_backend(_algorithm: EncryptionAlgorithm) -> Option<AeadBackend> {
    None
}

#[allow(unreachable_patterns)]
fn create_aes_128(key: [u8; 16]) -> Arc<dyn Encryptor> {
    let fallback = match preferred_aead_backend(EncryptionAlgorithm::AesGcm) {
        #[cfg(feature = "openssl-crypto")]
        Some(AeadBackend::OpenSsl) => Arc::new(openssl::OpenSslCipher::new_aes128_gcm(key)),
        #[cfg(all(not(feature = "openssl-crypto"), feature = "ring-crypto"))]
        Some(AeadBackend::Ring) => Arc::new(ring::RingCipher::new_aes128_gcm(key)),
        #[cfg(all(
            not(feature = "openssl-crypto"),
            not(feature = "ring-crypto"),
            feature = "aes-gcm"
        ))]
        Some(AeadBackend::RustCrypto) => Arc::new(aes_gcm::AesGcmCipher::new_128(key)),
        _ => unavailable_encryptor("aes-gcm"),
    };
    maybe_offload_aead(EncryptionAlgorithm::AesGcm, &key, fallback)
}

#[allow(unreachable_patterns)]
fn create_aes_256(key: [u8; 32]) -> Arc<dyn Encryptor> {
    let fallback = match preferred_aead_backend(EncryptionAlgorithm::Aes256Gcm) {
        #[cfg(feature = "openssl-crypto")]
        Some(AeadBackend::OpenSsl) => Arc::new(openssl::OpenSslCipher::new_aes256_gcm(key)),
        #[cfg(all(not(feature = "openssl-crypto"), feature = "ring-crypto"))]
        Some(AeadBackend::Ring) => Arc::new(ring::RingCipher::new_aes256_gcm(key)),
        #[cfg(all(
            not(feature = "openssl-crypto"),
            not(feature = "ring-crypto"),
            feature = "aes-gcm"
        ))]
        Some(AeadBackend::RustCrypto) => Arc::new(aes_gcm::AesGcmCipher::new_256(key)),
        _ => unavailable_encryptor("aes-256-gcm"),
    };
    maybe_offload_aead(EncryptionAlgorithm::Aes256Gcm, &key, fallback)
}

#[allow(unreachable_patterns)]
fn create_chacha20(key: [u8; 32]) -> Arc<dyn Encryptor> {
    let fallback = match preferred_aead_backend(EncryptionAlgorithm::ChaCha20) {
        #[cfg(feature = "openssl-crypto")]
        Some(AeadBackend::OpenSsl) => Arc::new(openssl::OpenSslCipher::new_chacha20(key)),
        #[cfg(all(not(feature = "openssl-crypto"), feature = "ring-crypto"))]
        Some(AeadBackend::Ring) => Arc::new(ring::RingCipher::new_chacha20(key)),
        #[cfg(all(
            not(feature = "openssl-crypto"),
            not(feature = "ring-crypto"),
            feature = "chacha20"
        ))]
        Some(AeadBackend::RustCrypto) => Arc::new(chacha20::ChaCha20Cipher::new(key)),
        _ => unavailable_encryptor("chacha20"),
    };
    maybe_offload_aead(EncryptionAlgorithm::ChaCha20, &key, fallback)
}

#[cfg(all(target_os = "wasi", feature = "wasi-crypto-offload"))]
fn maybe_offload_aead(
    algorithm: EncryptionAlgorithm,
    key: &[u8],
    fallback: Arc<dyn Encryptor>,
) -> Arc<dyn Encryptor> {
    Arc::new(wasi_host::WasiHostAead::new(algorithm, key, fallback))
}

#[cfg(not(all(target_os = "wasi", feature = "wasi-crypto-offload")))]
fn maybe_offload_aead(
    _algorithm: EncryptionAlgorithm,
    _key: &[u8],
    fallback: Arc<dyn Encryptor>,
) -> Arc<dyn Encryptor> {
    fallback
}

pub(crate) fn validate_algorithm(algorithm: &str) -> Result<(), Error> {
    let parsed = algorithm
        .parse::<EncryptionAlgorithm>()
        .map_err(|_| Error::InvalidAlgorithm(algorithm.to_owned()))?;
    if algorithm_is_available(parsed) {
        Ok(())
    } else {
        Err(Error::AlgorithmUnavailable(parsed.to_string()))
    }
}

pub(super) fn effective_algorithm_uses_xor(algorithm: &str) -> bool {
    algorithm.parse() == Ok(EncryptionAlgorithm::Xor)
}

/// Create an encryptor based on the algorithm name.
///
/// Callers that accept user configuration validate it during construction.
/// Protocol paths remain infallible here and receive an encryptor that returns
/// an explicit error if a peer names an invalid or unavailable algorithm.
pub fn create_encryptor(
    algorithm: &str,
    key_128: [u8; 16],
    key_256: [u8; 32],
) -> Arc<dyn Encryptor> {
    let Ok(algorithm) = algorithm.parse::<EncryptionAlgorithm>() else {
        return invalid_encryptor(algorithm);
    };

    match algorithm {
        EncryptionAlgorithm::Xor => Arc::new(xor::XorCipher::new(&key_128)),
        EncryptionAlgorithm::AesGcm => create_aes_128(key_128),
        EncryptionAlgorithm::Aes256Gcm => create_aes_256(key_256),
        EncryptionAlgorithm::ChaCha20 => create_chacha20(key_256),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{StandardAeadTail, ZCPacket};

    fn assert_interoperable(left: &dyn Encryptor, right: &dyn Encryptor) {
        let plaintext = b"cross-backend compatibility";
        let nonce = [9; StandardAeadTail::NONCE_SIZE];
        let mut left_packet = ZCPacket::new_with_payload(plaintext);
        left_packet.fill_peer_manager_hdr(1, 2, 1);
        let mut right_packet = ZCPacket::new_with_payload(plaintext);
        right_packet.fill_peer_manager_hdr(1, 2, 1);

        left.encrypt_with_nonce(&mut left_packet, Some(&nonce))
            .unwrap();
        right
            .encrypt_with_nonce(&mut right_packet, Some(&nonce))
            .unwrap();
        assert_eq!(left_packet.payload(), right_packet.payload());

        left.decrypt(&mut right_packet).unwrap();
        right.decrypt(&mut left_packet).unwrap();
        assert_eq!(left_packet.payload(), plaintext);
        assert_eq!(right_packet.payload(), plaintext);
    }

    #[test]
    fn network_secret_key_derivation_is_stable() {
        assert_eq!(
            derive_key_128("secret"),
            [
                2, 206, 42, 192, 32, 157, 49, 107, 163, 60, 45, 18, 4, 252, 117, 40,
            ]
        );
        assert_eq!(
            derive_key_256("secret"),
            [
                107, 17, 127, 149, 213, 198, 67, 158, 152, 97, 93, 210, 152, 104, 19, 110, 119,
                201, 239, 74, 160, 127, 3, 243, 98, 125, 135, 170, 175, 33, 208, 167,
            ]
        );
    }

    #[test]
    fn effective_algorithm_only_reports_explicit_xor() {
        assert!(effective_algorithm_uses_xor("xor"));
        assert!(!effective_algorithm_uses_xor(""));
        assert!(!effective_algorithm_uses_xor("unsupported"));
        assert!(!effective_algorithm_uses_xor("aes-gcm"));
    }

    #[cfg(not(any(
        feature = "aes-gcm",
        feature = "openssl-crypto",
        feature = "ring-crypto"
    )))]
    #[test]
    fn unavailable_aes_is_known_but_rejected() {
        assert_eq!(
            validate_algorithm("aes-gcm").unwrap_err().to_string(),
            "encryption algorithm is unavailable in this build: aes-gcm"
        );
    }

    #[cfg(any(
        feature = "aes-gcm",
        feature = "openssl-crypto",
        feature = "ring-crypto"
    ))]
    #[test]
    fn compiled_aes_is_available() {
        validate_algorithm("aes-gcm").unwrap();
        validate_algorithm("aes-256-gcm").unwrap();
    }

    #[cfg(not(any(
        feature = "chacha20",
        feature = "openssl-crypto",
        feature = "ring-crypto"
    )))]
    #[test]
    fn unavailable_chacha20_is_known_but_rejected() {
        assert_eq!(
            validate_algorithm("chacha20-poly1305")
                .unwrap_err()
                .to_string(),
            "encryption algorithm is unavailable in this build: chacha20"
        );
    }

    #[cfg(any(
        feature = "chacha20",
        feature = "openssl-crypto",
        feature = "ring-crypto"
    ))]
    #[test]
    fn compiled_chacha20_is_available() {
        validate_algorithm("chacha20").unwrap();
    }

    #[test]
    fn accelerated_backends_take_precedence_over_rustcrypto() {
        #[cfg(feature = "openssl-crypto")]
        assert_eq!(
            preferred_aead_backend(EncryptionAlgorithm::AesGcm),
            Some(AeadBackend::OpenSsl)
        );

        #[cfg(all(not(feature = "openssl-crypto"), feature = "ring-crypto"))]
        assert_eq!(
            preferred_aead_backend(EncryptionAlgorithm::AesGcm),
            Some(AeadBackend::Ring)
        );

        #[cfg(all(
            not(feature = "openssl-crypto"),
            not(feature = "ring-crypto"),
            feature = "aes-gcm"
        ))]
        assert_eq!(
            preferred_aead_backend(EncryptionAlgorithm::AesGcm),
            Some(AeadBackend::RustCrypto)
        );
    }

    #[cfg(all(feature = "ring-crypto", feature = "aes-gcm"))]
    #[test]
    fn ring_and_rustcrypto_aes_are_interoperable() {
        assert_interoperable(
            &ring::RingCipher::new_aes128_gcm([1; 16]),
            &aes_gcm::AesGcmCipher::new_128([1; 16]),
        );
        assert_interoperable(
            &ring::RingCipher::new_aes256_gcm([2; 32]),
            &aes_gcm::AesGcmCipher::new_256([2; 32]),
        );
    }

    #[cfg(all(feature = "ring-crypto", feature = "chacha20"))]
    #[test]
    fn ring_and_rustcrypto_chacha20_are_interoperable() {
        assert_interoperable(
            &ring::RingCipher::new_chacha20([3; 32]),
            &chacha20::ChaCha20Cipher::new([3; 32]),
        );
    }

    #[cfg(all(feature = "openssl-crypto", feature = "aes-gcm"))]
    #[test]
    fn openssl_and_rustcrypto_aes_are_interoperable() {
        assert_interoperable(
            &openssl::OpenSslCipher::new_aes128_gcm([1; 16]),
            &aes_gcm::AesGcmCipher::new_128([1; 16]),
        );
        assert_interoperable(
            &openssl::OpenSslCipher::new_aes256_gcm([2; 32]),
            &aes_gcm::AesGcmCipher::new_256([2; 32]),
        );
    }

    #[cfg(all(feature = "openssl-crypto", feature = "chacha20"))]
    #[test]
    fn openssl_and_rustcrypto_chacha20_are_interoperable() {
        assert_interoperable(
            &openssl::OpenSslCipher::new_chacha20([3; 32]),
            &chacha20::ChaCha20Cipher::new([3; 32]),
        );
    }

    #[cfg(all(feature = "openssl-crypto", feature = "ring-crypto"))]
    #[test]
    fn openssl_and_ring_algorithms_are_interoperable() {
        assert_interoperable(
            &openssl::OpenSslCipher::new_aes128_gcm([1; 16]),
            &ring::RingCipher::new_aes128_gcm([1; 16]),
        );
        assert_interoperable(
            &openssl::OpenSslCipher::new_aes256_gcm([2; 32]),
            &ring::RingCipher::new_aes256_gcm([2; 32]),
        );
        assert_interoperable(
            &openssl::OpenSslCipher::new_chacha20([3; 32]),
            &ring::RingCipher::new_chacha20([3; 32]),
        );
    }

    #[test]
    fn invalid_algorithm_is_rejected() {
        assert_eq!(
            validate_algorithm("rot13").unwrap_err().to_string(),
            "invalid encryption algorithm: rot13"
        );
    }
}
