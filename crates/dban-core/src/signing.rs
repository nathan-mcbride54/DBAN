//! Ed25519 detached signatures for erasure reports.
//!
//! Compliance use cases need a tamper-evident artifact: proof that the report
//! you are holding is byte-for-byte what DBAN produced at the end of the
//! session. We sign the exact bytes of the report file with an Ed25519 key and
//! write a detached `.sig` sidecar carrying the public key and signature.
//!
//! The key is **ephemeral** — generated fresh per session from OS entropy. That
//! is deliberate: it proves the report has not been altered since it was
//! written (anyone can verify with the embedded public key), without DBAN
//! having to manage long-lived secrets on a live boot medium. For provenance
//! tying reports to a known signer, export the public key out-of-band.
//!
//! Verification (any Ed25519 tool, e.g. with the bundled public key):
//! the signature covers the raw bytes of `signed_file`; recompute and compare.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde::Serialize;

/// A detached signature over a report file, serialized to the `.sig` sidecar.
#[derive(Clone, Debug, Serialize)]
pub struct ReportSignature {
    /// Always `"Ed25519"`.
    pub algorithm: &'static str,
    /// Name of the file these bytes were signed from.
    pub signed_file: String,
    /// 32-byte Ed25519 public key, hex-encoded (64 chars).
    pub public_key: String,
    /// 64-byte Ed25519 signature, hex-encoded (128 chars).
    pub signature: String,
    /// Human note on how to verify.
    pub note: &'static str,
}

impl ReportSignature {
    /// Sign `bytes` (the exact contents of `signed_file`) with a fresh
    /// ephemeral key and return the detached signature record.
    pub fn create(signed_file: &str, bytes: &[u8]) -> Self {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("operating system entropy unavailable");
        let signing_key = SigningKey::from_bytes(&seed);
        let signature = signing_key.sign(bytes);
        ReportSignature {
            algorithm: "Ed25519",
            signed_file: signed_file.to_string(),
            public_key: hex(signing_key.verifying_key().as_bytes()),
            signature: hex(&signature.to_bytes()),
            note: "Ed25519 detached signature over the raw bytes of signed_file. \
                   Verify with the public_key above.",
        }
    }

    /// Pretty-printed JSON for the `.sig` sidecar file.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("signature serialization cannot fail")
    }

    /// A short fingerprint of the public key (first 16 hex chars) for display.
    pub fn fingerprint(&self) -> String {
        self.public_key.chars().take(16).collect()
    }

    /// Verify `bytes` against this record's public key and signature. Used by
    /// tests and any external auditor that re-implements the same check.
    pub fn verify(&self, bytes: &[u8]) -> bool {
        let (Some(pk), Some(sig)) = (
            unhex(&self.public_key).and_then(|b| <[u8; 32]>::try_from(b).ok()),
            unhex(&self.signature).and_then(|b| <[u8; 64]>::try_from(b).ok()),
        ) else {
            return false;
        };
        let Ok(key) = VerifyingKey::from_bytes(&pk) else {
            return false;
        };
        key.verify_strict(bytes, &ed25519_dalek::Signature::from_bytes(&sig))
            .is_ok()
    }
}

/// Lower-case hex encoding.
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Decode a lower/upper-case hex string, or `None` if malformed.
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    fn nib(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push((nib(pair[0])? << 4) | nib(pair[1])?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let data = [0x00u8, 0x0f, 0xa5, 0xff, 0x10];
        assert_eq!(hex(&data), "000fa5ff10");
        assert_eq!(unhex("000FA5ff10").unwrap(), data);
        assert!(unhex("xyz").is_none());
        assert!(unhex("abc").is_none()); // odd length
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let bytes = b"erasure report contents";
        let sig = ReportSignature::create("dban-report-1.json", bytes);
        assert_eq!(sig.algorithm, "Ed25519");
        assert_eq!(sig.public_key.len(), 64);
        assert_eq!(sig.signature.len(), 128);
        assert!(sig.verify(bytes), "fresh signature must verify");
        assert_eq!(sig.fingerprint().len(), 16);
    }

    #[test]
    fn tampering_breaks_verification() {
        let bytes = b"erasure report contents";
        let sig = ReportSignature::create("r.json", bytes);
        let mut tampered = bytes.to_vec();
        tampered[0] ^= 0x01;
        assert!(
            !sig.verify(&tampered),
            "altered bytes must fail verification"
        );
    }

    #[test]
    fn distinct_sessions_use_distinct_keys() {
        let a = ReportSignature::create("r.json", b"x");
        let b = ReportSignature::create("r.json", b"x");
        assert_ne!(a.public_key, b.public_key, "keys are ephemeral per call");
    }

    #[test]
    fn json_sidecar_is_wellformed() {
        let sig = ReportSignature::create("r.json", b"data");
        let v: serde_json::Value = serde_json::from_str(&sig.to_json()).unwrap();
        assert_eq!(v["algorithm"], "Ed25519");
        assert_eq!(v["signed_file"], "r.json");
        assert!(v["public_key"].as_str().unwrap().len() == 64);
    }
}
