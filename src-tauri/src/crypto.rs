use chacha20poly1305::aead::{Aead, AeadCore, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::error::Error;

pub struct SpakeState {
    spake: Spake2<Ed25519Group>,
}

pub fn start_spake2(
    password: &str,
    _id_a: &str,
    _id_b: &str,
) -> Result<(SpakeState, Vec<u8>), Box<dyn Error + Send + Sync>> {
    let (spake, msg) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(password.as_bytes()),
        &Identity::new(b"clustercut-connect"),
    );

    Ok((SpakeState { spake }, msg))
}

pub fn finish_spake2(
    state: SpakeState,
    inbound_msg: &[u8],
) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let key = state
        .spake
        .finish(inbound_msg)
        .map_err(|e| format!("Spake error: {}", e))?;
    Ok(key)
}

pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng); // 96-bits; unique per message
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| format!("Encryption failure: {}", e))?;

    let mut result = nonce.to_vec();
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

pub fn decrypt(
    key: &[u8; 32],
    ciphertext_with_nonce: &[u8],
) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    if ciphertext_with_nonce.len() < 12 {
        return Err("Ciphertext too short".into());
    }

    let nonce = &ciphertext_with_nonce[..12];
    let ciphertext = &ciphertext_with_nonce[12..];

    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let plaintext = cipher
        .decrypt(nonce.into(), ciphertext)
        .map_err(|e| format!("Decryption failure: {}", e))?;

    Ok(plaintext)
}
