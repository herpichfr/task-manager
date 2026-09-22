//! Argon2id key derivation for locked boards. A locked board's SQLCipher
//! file is keyed with a raw 32-byte key (see `storage::locked_db`), and
//! that key is always this module's output -- SQLCipher's own built-in KDF
//! is never used, so the cost parameters and salt are ours to choose and
//! store per board.

use argon2::{Algorithm, Argon2, Params, Version};
use rand::rngs::OsRng;
use rand::TryRngCore;
use zeroize::Zeroizing;

use crate::error::AppError;

/// Argon2id cost parameters. `m_cost` is in KiB. The `Default` impl is the
/// production setting (~64 MiB, 3 passes, 1 lane); tests use a much
/// cheaper `Kdf` so the suite stays fast.
#[derive(Debug, Clone, Copy)]
pub struct Kdf {
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
}

impl Default for Kdf {
    fn default() -> Self {
        Kdf { m_cost: 65536, t_cost: 3, p_cost: 1 }
    }
}

/// Generates a fresh random 16-byte salt using the OS RNG.
pub fn generate_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    OsRng.try_fill_bytes(&mut salt).expect("OS RNG failure");
    salt
}

/// Derives a 32-byte key from `passphrase` and `salt` using Argon2id via
/// the raw-KDF API (`hash_password_into`), not the PHC string format. The
/// key is returned in a `Zeroizing` buffer and must never be logged,
/// stored, or embedded in an error message.
pub fn derive_key(passphrase: &str, salt: &[u8], kdf: Kdf) -> Result<Zeroizing<[u8; 32]>, AppError> {
    let params = Params::new(kdf.m_cost, kdf.t_cost, kdf.p_cost, Some(32))
        .map_err(|e| AppError::Crypto(e.to_string()))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|e| AppError::Crypto(e.to_string()))?;
    Ok(key)
}

/// Renders a 32-byte key as 64 lowercase hex characters, the form SQLCipher
/// expects in `PRAGMA key = "x'<hex>'"`.
pub fn key_to_hex(key: &[u8; 32]) -> Zeroizing<String> {
    let mut s = String::with_capacity(64);
    for b in key.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    Zeroizing::new(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny cost parameters so the suite stays fast; production uses
    /// `Kdf::default()`.
    fn tiny_kdf() -> Kdf {
        Kdf { m_cost: 8, t_cost: 1, p_cost: 1 }
    }

    #[test]
    fn deterministic_for_same_passphrase_and_salt() {
        let salt = generate_salt();
        let k1 = derive_key("hunter2", &salt, tiny_kdf()).unwrap();
        let k2 = derive_key("hunter2", &salt, tiny_kdf()).unwrap();
        assert_eq!(*k1, *k2);
    }

    #[test]
    fn different_salt_gives_different_key() {
        let salt_a = generate_salt();
        let salt_b = generate_salt();
        let k1 = derive_key("hunter2", &salt_a, tiny_kdf()).unwrap();
        let k2 = derive_key("hunter2", &salt_b, tiny_kdf()).unwrap();
        assert_ne!(*k1, *k2);
    }

    #[test]
    fn hex_is_64_lowercase_chars() {
        let salt = generate_salt();
        let key = derive_key("hunter2", &salt, tiny_kdf()).unwrap();
        let hex = key_to_hex(&key);
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn wrong_passphrase_gives_different_key() {
        let salt = generate_salt();
        let k1 = derive_key("hunter2", &salt, tiny_kdf()).unwrap();
        let k2 = derive_key("wrong", &salt, tiny_kdf()).unwrap();
        assert_ne!(*k1, *k2);
    }
}
