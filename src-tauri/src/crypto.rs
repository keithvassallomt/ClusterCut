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

// Optimization: Stateful Encryptor/Decryptor for File Streaming
// Reuses the Cipher context and uses a simple counter for the nonce (Nonce = BaseIV + Counter)

pub struct StatefulEncryptor {
    cipher: ChaCha20Poly1305,
    base_nonce: [u8; 12],
    counter: u64,
}

impl StatefulEncryptor {
    pub fn new(key: &[u8; 32], iv: [u8; 12]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            base_nonce: iv,
            counter: 0,
        }
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        // Construct Nonce: BaseIV XOR Counter (or just add to last 8 bytes)
        // Simple strategy: Use first 4 bytes of IV, then 8 bytes of counter (Big Endian)
        // Or just XOR buffer. XOR is safe if IV is random and unique per file.
        let nonce = self.generate_nonce();

        // Encrypt
        let ciphertext = self
            .cipher
            .encrypt(&nonce.into(), plaintext)
            .map_err(|e| format!("Encryption failure: {}", e))?;

        self.counter += 1;
        Ok(ciphertext)
    }

    fn generate_nonce(&self) -> [u8; 12] {
        let mut n = self.base_nonce;
        // Add counter to the last 8 bytes (little endian)
        let mut c = self.counter;
        for i in 0..8 {
            let val = (c & 0xFF) as u8;
            // n[4 + i] usually safe. Base nonce is 12 bytes.
            // Using XOR allows full 12 byte usage if we wanted, but affecting last 8 is standard for 96-bit nonces.
            n[4 + i] ^= val;
            c >>= 8;
        }
        n
    }
}

pub struct StatefulDecryptor {
    cipher: ChaCha20Poly1305,
    base_nonce: [u8; 12],
    counter: u64,
}

impl StatefulDecryptor {
    pub fn new(key: &[u8; 32], iv: [u8; 12]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            base_nonce: iv,
            counter: 0,
        }
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let nonce = self.generate_nonce();

        let plaintext = self
            .cipher
            .decrypt(&nonce.into(), ciphertext)
            .map_err(|e| format!("Decryption failure: {}", e))?;

        self.counter += 1;
        Ok(plaintext)
    }

    fn generate_nonce(&self) -> [u8; 12] {
        let mut n = self.base_nonce;
        let mut c = self.counter;
        for i in 0..8 {
            let val = (c & 0xFF) as u8;
            n[4 + i] ^= val;
            c >>= 8;
        }
        n
    }
}
