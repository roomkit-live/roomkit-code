//! SHA-256 digests spelled in lowercase hexadecimal.
//!
//! Several of these strings name stored state: a project's directory, its
//! session locks, plans, images and shell events. They keep the spelling
//! sha2 0.10's `{:x}` gave, every byte as two lowercase digits; sha2 0.11's
//! digest has no `LowerHex`.

use std::fmt::Write;

use sha2::{Digest, Sha256};

/// The SHA-256 digest of `data` in lowercase hexadecimal.
pub(crate) fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    hex(&Sha256::digest(data))
}

/// `bytes` in lowercase hexadecimal, two digits each.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_keep_the_lowercase_spelling_of_stored_names() {
        // FIPS 180-2 vectors, as `sha256sum` prints them.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hex(&[0, 1, 0xab, 0xff]), "0001abff");
    }
}
