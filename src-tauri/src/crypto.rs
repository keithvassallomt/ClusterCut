use chacha20poly1305::aead::{Aead, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::{Digest, Sha256};
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

// ────────────────────────────────────────────────────────────────────────────
// Wire-protocol 0.3.1 pairing primitives
//
// SPAKE2 derives a shared 32-byte key K. From K + the role-labelled SPAKE2
// transcript we derive two direction-distinct AEAD sub-keys (initiator→responder
// and responder→initiator) via HKDF-Expand. Each T2/T3 frame is then a single
// ChaCha20-Poly1305 ciphertext over the serialised inner struct under the
// appropriate sub-key. Decryption failure aborts pairing — no field on the wire
// is trusted before the AEAD tag verifies.
//
// The transcript bytes never travel on the wire: both sides reconstruct them
// independently from the two SPAKE2 messages they observed. Any rewrite of a
// SPAKE2 byte by a network attacker diverges the reconstructed transcripts,
// diverges the sub-keys, and AEAD decryption fails closed on both ends.
// ────────────────────────────────────────────────────────────────────────────

const TRANSCRIPT_DOMAIN: &[u8] = b"clustercut-pair-v1";
const INITIATOR_LABEL: &[u8] = b"initiator";
const RESPONDER_LABEL: &[u8] = b"responder";
const HKDF_INFO_I2R: &[u8] = b"clustercut-pair-v1 i2r";
const HKDF_INFO_R2I: &[u8] = b"clustercut-pair-v1 r2i";

/// SHA-256 of `domain || "initiator" || spake_msg_i || "responder" || spake_msg_r`.
/// The fixed labels act as both role tags and component separators.
pub fn pairing_transcript(spake_msg_i: &[u8], spake_msg_r: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(TRANSCRIPT_DOMAIN);
    h.update(INITIATOR_LABEL);
    h.update(spake_msg_i);
    h.update(RESPONDER_LABEL);
    h.update(spake_msg_r);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// Derive the two direction-distinct AEAD sub-keys from the SPAKE2 shared key
/// and the role-labelled transcript hash. The SPAKE2 output is already a
/// uniformly-random 32-byte secret, so we feed it straight into HKDF-Expand
/// as the PRK rather than re-extracting.
pub fn derive_pair_subkeys(
    session_key: &[u8],
    transcript: &[u8; 32],
) -> Result<([u8; 32], [u8; 32]), Box<dyn Error + Send + Sync>> {
    if session_key.len() != 32 {
        return Err("SPAKE2 session key must be 32 bytes".into());
    }
    let hk = Hkdf::<Sha256>::from_prk(session_key)
        .map_err(|e| format!("HKDF-from-prk failed: {}", e))?;

    let mut i2r = [0u8; 32];
    let mut r2i = [0u8; 32];

    let info_i2r = [HKDF_INFO_I2R, transcript.as_slice()].concat();
    let info_r2i = [HKDF_INFO_R2I, transcript.as_slice()].concat();

    hk.expand(&info_i2r, &mut i2r)
        .map_err(|e| format!("HKDF-expand i2r failed: {}", e))?;
    hk.expand(&info_r2i, &mut r2i)
        .map_err(|e| format!("HKDF-expand r2i failed: {}", e))?;

    Ok((i2r, r2i))
}

/// Generate a fresh random 96-bit nonce for a one-shot AEAD pairing frame.
/// Each pairing sub-key encrypts exactly one message; the nonce on the wire is
/// belt-and-braces (standard AEAD construction) rather than load-bearing.
pub fn fresh_pair_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

/// Encrypt one pairing-channel frame under `subkey` and the explicit `nonce`
/// the caller will transmit alongside the ciphertext.
pub fn pair_aead_encrypt(
    subkey: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(subkey));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(nonce), plaintext)
        .map_err(|e| format!("pair AEAD encrypt failed: {}", e))?;
    Ok(ciphertext)
}

/// Decrypt one pairing-channel frame. Returns the plaintext on tag-verify
/// success, or an opaque error on any failure — callers must treat failure as
/// "abort pairing" and never inspect the (untrusted) ciphertext otherwise.
pub fn pair_aead_decrypt(
    subkey: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(subkey));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|e| format!("pair AEAD decrypt failed: {}", e))?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_spake_msgs() -> (Vec<u8>, Vec<u8>) {
        // Length and content don't matter for these unit tests — we only need
        // two distinct byte strings standing in for spake_msg_i / spake_msg_r.
        (vec![0x11; 33], vec![0x22; 33])
    }

    #[test]
    fn transcript_role_order_matters() {
        let (i, r) = dummy_spake_msgs();
        let t1 = pairing_transcript(&i, &r);
        let t2 = pairing_transcript(&r, &i);
        // Swapping which msg is labelled "initiator" must produce a distinct
        // transcript — that's what kills the lex-sort vulnerability the spec
        // worked around.
        assert_ne!(t1, t2);
    }

    #[test]
    fn transcript_changes_on_any_byte_flip() {
        let (i, mut r) = dummy_spake_msgs();
        let baseline = pairing_transcript(&i, &r);
        r[0] ^= 0x01;
        let tampered = pairing_transcript(&i, &r);
        assert_ne!(baseline, tampered);
    }

    #[test]
    fn subkeys_are_distinct() {
        let k = [0x42u8; 32];
        let (i, r) = dummy_spake_msgs();
        let t = pairing_transcript(&i, &r);
        let (i2r, r2i) = derive_pair_subkeys(&k, &t).unwrap();
        assert_ne!(i2r, r2i);
    }

    #[test]
    fn subkeys_diverge_on_session_key_change() {
        let (i, r) = dummy_spake_msgs();
        let t = pairing_transcript(&i, &r);
        let k1 = [0x01u8; 32];
        let k2 = [0x02u8; 32];
        let (a, _) = derive_pair_subkeys(&k1, &t).unwrap();
        let (b, _) = derive_pair_subkeys(&k2, &t).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn pair_aead_round_trip() {
        let key = [0x33u8; 32];
        let nonce = fresh_pair_nonce();
        let plaintext = b"some-pair-inner-bytes";
        let ct = pair_aead_encrypt(&key, &nonce, plaintext).unwrap();
        let pt = pair_aead_decrypt(&key, &nonce, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn pair_aead_rejects_direction_swap() {
        // Encrypt under i2r, attempt to decrypt under r2i — must fail closed.
        let k = [0x55u8; 32];
        let (i, r) = dummy_spake_msgs();
        let t = pairing_transcript(&i, &r);
        let (i2r, r2i) = derive_pair_subkeys(&k, &t).unwrap();
        let nonce = fresh_pair_nonce();
        let ct = pair_aead_encrypt(&i2r, &nonce, b"hello").unwrap();
        let err = pair_aead_decrypt(&r2i, &nonce, &ct);
        assert!(err.is_err(), "swapped-direction decrypt must fail");
    }

    #[test]
    fn pair_aead_rejects_tampered_ciphertext() {
        let k = [0x77u8; 32];
        let nonce = fresh_pair_nonce();
        let mut ct = pair_aead_encrypt(&k, &nonce, b"payload").unwrap();
        ct[0] ^= 0x01;
        assert!(pair_aead_decrypt(&k, &nonce, &ct).is_err());
    }

    // ─── Full-handshake security regression tests (WIRE-PROTOCOL-0.3.1) ───
    //
    // These drive both halves of the pairing crypto flow in-process so we
    // can inject the attacker behaviours the doc's "Verification commitments"
    // call out without standing up real TCP machinery:
    //   - Wrong-PIN  (mismatched session keys → AEAD must fail closed)
    //   - Transcript-tamper (rewrite a SPAKE2 byte in flight → sub-keys
    //     diverge → AEAD fails)
    //   - MITM rewrite of T2/T3 ciphertext (must fail closed)
    //   - Direction-swap (initiator-direction frame replayed as responder-
    //     direction → sub-keys differ → fail)

    /// Run both SPAKE2 halves with the given PINs, derive the role-distinct
    /// sub-keys on each side from the messages they observed (allowing the
    /// caller to swap bytes mid-flight to model an attacker), and return:
    ///   - Initiator's (k_i2r, k_r2i)
    ///   - Responder's (k_i2r, k_r2i)
    /// Plus the raw SPAKE2 messages so callers can splice them.
    fn run_spake2_pair(pin_initiator: &str, pin_responder: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let (s_i, msg_i) = start_spake2(pin_initiator, "init", "resp").unwrap();
        let (s_r, msg_r) = start_spake2(pin_responder, "resp", "init").unwrap();
        let k_i = finish_spake2(s_i, &msg_r).unwrap();
        let k_r = finish_spake2(s_r, &msg_i).unwrap();
        (msg_i, msg_r, k_i, k_r)
    }

    #[test]
    fn wrong_pin_makes_aead_fail_closed() {
        let (msg_i, msg_r, k_init, k_resp) = run_spake2_pair("pin-a", "pin-b");
        let t = pairing_transcript(&msg_i, &msg_r);
        let (k_i2r_init, _) = derive_pair_subkeys(&k_init, &t).unwrap();
        let (_, k_r2i_resp) = derive_pair_subkeys(&k_resp, &t).unwrap();

        // Initiator encrypts T3 under their k_i2r; responder tries to decrypt
        // under their k_i2r — but with wrong PINs the SPAKE2 keys diverge,
        // so sub-keys diverge too, so AEAD must fail.
        let (k_i2r_resp, _) = derive_pair_subkeys(&k_resp, &t).unwrap();
        assert_ne!(k_i2r_init, k_i2r_resp, "wrong-PIN initiator sub-key must differ from responder's");
        let _ = k_r2i_resp; // unused; here for clarity that both sub-keys differ

        let nonce = fresh_pair_nonce();
        let ct = pair_aead_encrypt(&k_i2r_init, &nonce, b"InitiatorId-inner").unwrap();
        let result = pair_aead_decrypt(&k_i2r_resp, &nonce, &ct);
        assert!(result.is_err(), "wrong-PIN AEAD decrypt must fail closed");
    }

    #[test]
    fn transcript_tamper_makes_aead_fail_closed() {
        // Both sides used the right PIN, so their SPAKE2 keys agree — but
        // the network attacker flipped a byte of msg_r in flight, so the two
        // sides reconstruct different transcripts and thus different sub-keys.
        let (msg_i, mut msg_r_real, k_init, k_resp) = run_spake2_pair("same-pin", "same-pin");
        assert_eq!(k_init, k_resp, "matching PINs must agree on session key");
        // The initiator observes a tampered msg_r; responder observes the real one.
        let mut msg_r_tampered = msg_r_real.clone();
        msg_r_tampered[0] ^= 0x01;
        // ...so each side derives a different transcript:
        let t_init = pairing_transcript(&msg_i, &msg_r_tampered);
        let t_resp = pairing_transcript(&msg_i, &msg_r_real);
        assert_ne!(t_init, t_resp);

        let (_, k_r2i_init) = derive_pair_subkeys(&k_init, &t_init).unwrap();
        let (_, k_r2i_resp) = derive_pair_subkeys(&k_resp, &t_resp).unwrap();
        assert_ne!(k_r2i_init, k_r2i_resp);

        let nonce = fresh_pair_nonce();
        let ct = pair_aead_encrypt(&k_r2i_resp, &nonce, b"ResponderId-inner").unwrap();
        // The initiator tries to decrypt under their (different) k_r2i — must fail.
        let attempt = pair_aead_decrypt(&k_r2i_init, &nonce, &ct);
        assert!(attempt.is_err(), "transcript-tamper must fail AEAD closed");
        // Touch msg_r_real to keep mut lint quiet on platforms where mut-write
        // analysis ignores indexing.
        let _ = &mut msg_r_real;
    }

    #[test]
    fn mitm_ciphertext_rewrite_fails_closed() {
        let (msg_i, msg_r, k_init, _k_resp) = run_spake2_pair("good-pin", "good-pin");
        let t = pairing_transcript(&msg_i, &msg_r);
        let (k_i2r, _) = derive_pair_subkeys(&k_init, &t).unwrap();
        let nonce = fresh_pair_nonce();
        let mut ct = pair_aead_encrypt(&k_i2r, &nonce, b"InitiatorId-inner").unwrap();
        // Flip a byte in the middle of the ciphertext (any tamper invalidates
        // the AEAD tag).
        let mid = ct.len() / 2;
        ct[mid] ^= 0x55;
        assert!(pair_aead_decrypt(&k_i2r, &nonce, &ct).is_err());
    }

    #[test]
    fn direction_swap_fails_closed() {
        // Encrypt a T3-shaped frame under k_i2r, then try to "deliver" it as
        // a T2 frame (i.e. the receiver decrypts under k_r2i). Must fail.
        let (msg_i, msg_r, k, _) = run_spake2_pair("p", "p");
        let t = pairing_transcript(&msg_i, &msg_r);
        let (k_i2r, k_r2i) = derive_pair_subkeys(&k, &t).unwrap();
        let nonce = fresh_pair_nonce();
        let ct = pair_aead_encrypt(&k_i2r, &nonce, b"InitiatorId-inner").unwrap();
        assert!(pair_aead_decrypt(&k_r2i, &nonce, &ct).is_err());
    }

    #[test]
    fn happy_path_round_trip_succeeds_with_matching_pin() {
        // Sanity: the same flow that fails for the attacks above MUST succeed
        // for honest peers with matching PINs and an untampered transcript.
        let (msg_i, msg_r, k_init, k_resp) = run_spake2_pair("hello", "hello");
        assert_eq!(k_init, k_resp);
        let t = pairing_transcript(&msg_i, &msg_r);
        let (k_i2r_init, k_r2i_init) = derive_pair_subkeys(&k_init, &t).unwrap();
        let (k_i2r_resp, k_r2i_resp) = derive_pair_subkeys(&k_resp, &t).unwrap();
        assert_eq!(k_i2r_init, k_i2r_resp);
        assert_eq!(k_r2i_init, k_r2i_resp);

        // T2: responder → initiator
        let nonce_r = fresh_pair_nonce();
        let ct_r = pair_aead_encrypt(&k_r2i_resp, &nonce_r, b"ResponderId-inner").unwrap();
        let pt_r = pair_aead_decrypt(&k_r2i_init, &nonce_r, &ct_r).unwrap();
        assert_eq!(pt_r, b"ResponderId-inner");

        // T3: initiator → responder
        let nonce_i = fresh_pair_nonce();
        let ct_i = pair_aead_encrypt(&k_i2r_init, &nonce_i, b"InitiatorId-inner").unwrap();
        let pt_i = pair_aead_decrypt(&k_i2r_resp, &nonce_i, &ct_i).unwrap();
        assert_eq!(pt_i, b"InitiatorId-inner");
    }
}
