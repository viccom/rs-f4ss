use std::fs::File;
use std::io::Read;
use std::path::Path;

use minisign_verify::{PublicKey, Signature};

use crate::error::Error;

/// Parse a Minisign public key.
///
/// Accepts two formats:
/// - Bare base64 (single line, 56 chars), e.g.
///   `RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3`
/// - Full `minisign.pub` content (untrusted comment + base64 line)
pub fn parse_public_key(pubkey: &str) -> Result<PublicKey, Error> {
    let trimmed = pubkey.trim();
    if trimmed.lines().count() >= 2 {
        PublicKey::decode(trimmed).map_err(|e| Error::InvalidPublicKey(e.to_string()))
    } else {
        PublicKey::from_base64(trimmed).map_err(|e| Error::InvalidPublicKey(e.to_string()))
    }
}

/// Parse a Minisign signature (4-line `.minisig` content).
pub fn parse_signature(sig: &str) -> Result<Signature, Error> {
    Signature::decode(sig.trim()).map_err(|e| Error::InvalidSignature(e.to_string()))
}

/// Verify a file against a Minisign signature using a public key.
///
/// Uses streaming verification so that multi-hundred-MB binaries don't have to
/// be loaded into memory. Only pre-hashed signatures (Minisign default since
/// version 0.6) are accepted — legacy mode is rejected because it requires
/// buffering the entire file.
pub fn verify_file(
    path: &Path,
    signature: &Signature,
    public_key: &PublicKey,
) -> Result<(), Error> {
    let mut verifier = public_key
        .verify_stream(signature)
        .map_err(|e| Error::InvalidSignature(e.to_string()))?;

    let mut file = File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        verifier.update(&buf[..n]);
    }
    verifier
        .finalize()
        .map_err(|e| Error::InvalidSignature(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector from minisign-verify upstream tests.
    // Public key: RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3
    // Signs "test" with pre-hashed mode.
    const TEST_PUBKEY_B64: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    const TEST_PUBKEY_FULL: &str = "untrusted comment: minisign public key E7620F1842B4E81F\n\
         RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    const TEST_SIG: &str =
        "untrusted comment: signature from minisign secret key\n\
         RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\n\
         trusted comment: timestamp:1556193335\tfile:test\n\
         y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

    #[test]
    fn parse_pubkey_bare_base64() {
        parse_public_key(TEST_PUBKEY_B64).unwrap();
    }

    #[test]
    fn parse_pubkey_full_format() {
        parse_public_key(TEST_PUBKEY_FULL).unwrap();
    }

    #[test]
    fn parse_pubkey_invalid() {
        let err = parse_public_key("not-a-key").unwrap_err();
        assert!(matches!(err, Error::InvalidPublicKey(_)));
    }

    #[test]
    fn parse_sig_ok() {
        parse_signature(TEST_SIG).unwrap();
    }

    #[test]
    fn parse_sig_invalid() {
        match parse_signature("garbage") {
            Err(Error::InvalidSignature(_)) => {}
            other => panic!("expected InvalidSignature, got {:?}", other.is_err()),
        }
    }

    #[test]
    fn verify_file_valid() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"test").unwrap();
        let pubkey = parse_public_key(TEST_PUBKEY_B64).unwrap();
        let sig = parse_signature(TEST_SIG).unwrap();
        verify_file(tmp.path(), &sig, &pubkey).unwrap();
    }

    #[test]
    fn verify_file_tampered() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"Test").unwrap(); // wrong content
        let pubkey = parse_public_key(TEST_PUBKEY_B64).unwrap();
        let sig = parse_signature(TEST_SIG).unwrap();
        let err = verify_file(tmp.path(), &sig, &pubkey).unwrap_err();
        assert!(matches!(err, Error::InvalidSignature(_)));
    }

    #[test]
    fn verify_file_wrong_key() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"test").unwrap();
        // Different valid key — should fail with UnexpectedKeyId
        let other_pubkey = "RWSGOq2NVecA2UPNdBUZykf1CCb147pkmdtYxgb3Ti+JO/wCYvhbWY9i";
        let pubkey = parse_public_key(other_pubkey).unwrap();
        let sig = parse_signature(TEST_SIG).unwrap();
        let err = verify_file(tmp.path(), &sig, &pubkey).unwrap_err();
        assert!(matches!(err, Error::InvalidSignature(_)));
    }
}
