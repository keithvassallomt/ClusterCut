# Wire Protocol 0.3.3 — Key-Confirmation Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the SPAKE-pairing online brute-force budget-bypass by inserting an explicit initiator key-confirmation packet before the responder reveals its AEAD-encrypted identity; hard-break the wire by bumping `proto` to `0.3.3`; and tighten the existing version-mismatch warning system so out-of-date peers are visibly flagged before users attempt to pair.

**Architecture:** Insert a new T2 `InitiatorKC { nonce, ciphertext }` frame into the pairing flow — an AEAD-encrypted fixed-label payload encrypted under `k_i2r` (the existing initiator→responder sub-key derived from the SPAKE2 transcript). The responder must decrypt it cleanly *before* sending its own AEAD-encrypted identity; any tag failure trips the existing `record_pairing_aead_failure` counter that already gates the H1 lockout. Bump `CLUSTERCUT_PROTOCOL_VERSION` to `"0.3.3"`; raise the backend (`is_protocol_compatible`) and frontend (`MIN_COMPATIBLE_PROTOCOL`) floors in lockstep — the frontend floor currently lags backend by one minor, which we fix here. Add a pre-flight version check in `start_pairing` so discovered LAN peers below floor surface in the existing "Peer needs updating" modal *before* a TCP socket opens, instead of falling out as a generic pairing failure.

**Tech Stack:** Rust + Tauri (backend), React + TypeScript (frontend). No new dependencies.

---

## Wire Protocol 0.3.3 Reference

This plan is the sole spec for 0.3.3. Diff from 0.3.1:

```text
0.3.1 (current):
  T0  Initiator → Responder   PairRequest   { spake_msg }
  T1  Responder → Initiator   PairResponse  { spake_msg }
  T2  Responder → Initiator   ResponderId   { nonce, ciphertext = AEAD(k_r2i, nonce, inner) }
  T3  Initiator → Responder   InitiatorId   { nonce, ciphertext = AEAD(k_i2r, nonce, inner) }

0.3.3 (new):
  T0  Initiator → Responder   PairRequest   { spake_msg }
  T1  Responder → Initiator   PairResponse  { spake_msg }
  T2  Initiator → Responder   InitiatorKC   { nonce, ciphertext = AEAD(k_i2r, nonce, KC_PLAINTEXT) }   ← NEW
  T3  Responder → Initiator   ResponderId   { nonce, ciphertext = AEAD(k_r2i, nonce, inner) }          (was T2)
  T4  Initiator → Responder   InitiatorId   { nonce, ciphertext = AEAD(k_i2r, nonce, inner) }          (was T3)
```

Where `KC_PLAINTEXT = b"clustercut-pair-v2-init-kc"` — a fixed byte string. The AEAD tag, computed over `(k_i2r, nonce, KC_PLAINTEXT)`, effectively MACs the plaintext under the SPAKE2-derived sub-key. Any wrong PIN diverges `k_i2r` between the two sides → decryption fails closed → counter increments. The transcript is unchanged from 0.3.1 — sub-keys remain bound to `(spake_msg_i, spake_msg_r)`, so any byte-rewrite of T0/T1 still flips sub-keys and trips the AEAD-fail path.

### Why this closes the budget bypass

**Pre-0.3.3 attack:** attacker initiates T0, receives T1 + the responder's encrypted T2 (`ResponderId`, ciphertext under `k_r2i` derived from the *attacker's* guessed PIN). He disconnects without sending T3. The responder logs no AEAD failure (the failure was that he didn't send T3 — but we only count AEAD-tag failures), so no counter increment. The attacker brute-forces his guessed PIN against the captured `ResponderId` ciphertext offline, learning yes/no for one PIN value per cycle. Repeat unbounded.

**Post-0.3.3 defence:** the responder won't send `ResponderId` until after it has verified `InitiatorKC` under `k_i2r`. The attacker's options:
- Send a bogus `InitiatorKC` ciphertext → AEAD-decrypt fails under the responder's `k_i2r` (different SPAKE2 keys) → `record_pairing_aead_failure` → counter increments → 10 fails ⇒ H1 lockout.
- Don't send `InitiatorKC` → never receives `ResponderId` → no encrypted material to take offline.
- Send a different-variant frame (e.g., `InitiatorId`) at T2 → logged as wrong-variant (no counter) → connection closes → still no encrypted material returned.

Either way the attacker's budget is bounded. Importantly: wrong-variant frames do *not* trip the counter, so honest 0.3.1 clients hitting a 0.3.3 responder don't lock the listener out for the user.

### Compatibility

This is a hard wire break. A 0.3.3 responder receiving a 0.3.1 initiator's `InitiatorId` at the T2 slot sees a wrong-variant frame; `log_pairing_failure` fires and the connection closes (no lockout counter bump). A 0.3.3 initiator hitting a 0.3.1 responder will send `InitiatorKC` at T2, which the older responder reads where it expects `InitiatorId`, also a wrong-variant log + close. The pre-flight check in Task 8 catches LAN cases with mDNS `proto` data and surfaces the existing "Peer needs updating" modal before opening the socket — for Add-Remote (no mDNS), wire-level failure with a generic message remains the only signal.

---

## File Structure

**Modify:**
- [src-tauri/src/discovery.rs](src-tauri/src/discovery.rs) — bump `CLUSTERCUT_PROTOCOL_VERSION` to `"0.3.3"`; extend the doc block.
- [src-tauri/src/protocol.rs](src-tauri/src/protocol.rs) — add `PairingMessage::InitiatorKC { nonce, ciphertext }`; update the wire-protocol doc block.
- [src-tauri/src/crypto.rs](src-tauri/src/crypto.rs) — add `INITIATOR_KC_PLAINTEXT` constant; add regression tests for the new flow.
- [src-tauri/src/lib.rs](src-tauri/src/lib.rs) — responder: insert T2 `InitiatorKC` read between SPAKE2-finish and the T3 (renumbered) `ResponderId` send. Initiator: insert T2 `InitiatorKC` write between SPAKE2-finish and T3 `ResponderId` read. Bump version floor in `is_protocol_compatible`. Add pre-flight version check at the top of `start_pairing`.
- [src/App.tsx](src/App.tsx) — bump `MIN_COMPATIBLE_PROTOCOL` to `[0, 3, 3]`.
- [CHANGELOG.md](CHANGELOG.md) — terse `Security` and `Changed` entries under `## [Unreleased]`.

**No new files.**

---

## Task 1: Bump `CLUSTERCUT_PROTOCOL_VERSION` to `"0.3.3"`

**Files:**
- Modify: [src-tauri/src/discovery.rs:7-17](src-tauri/src/discovery.rs#L7-L17)

The mDNS `proto` TXT property is the load-bearing version signal. Bumping it does two things: 0.3.1 peers see the 0.3.3 advertiser as future-version and flag it (their amber-triangle code matches on exact version delta), and 0.3.3 peers using the new `is_protocol_compatible` floor refuse the older peer at the per-peer send path.

- [ ] **Step 1: Update the constant and the doc block**

Replace the existing block (currently lines 7–17):

```rust
/// Protocol-compatibility version advertised in the mDNS `proto` TXT
/// property. Bumped whenever a wire-format or transport-security change
/// breaks compatibility with older peers.
///
/// - 0.3.0: strict-mTLS transport + plaintext-payload model (first break
///   from 0.2.x).
/// - 0.3.1: pairing-channel hardening per WIRE-PROTOCOL-0.3.1.md
///   (AEAD-wrapped identity in T2/T3, role-labelled SPAKE2 transcript,
///   `Welcome` deferred to QUIC/mTLS `ClusterInfo` exchange). Wire-
///   incompatible with 0.3.0; surfaced in the UI as "please upgrade".
pub const CLUSTERCUT_PROTOCOL_VERSION: &str = "0.3.1";
```

with:

```rust
/// Protocol-compatibility version advertised in the mDNS `proto` TXT
/// property. Bumped whenever a wire-format or transport-security change
/// breaks compatibility with older peers.
///
/// - 0.3.0: strict-mTLS transport + plaintext-payload model (first break
///   from 0.2.x).
/// - 0.3.1: pairing-channel hardening (AEAD-wrapped identity in T2/T3,
///   role-labelled SPAKE2 transcript, `Welcome` deferred to QUIC/mTLS
///   `ClusterInfo` exchange). Wire-incompatible with 0.3.0.
/// - 0.3.3: explicit initiator key-confirmation packet (new T2
///   `InitiatorKC`) ahead of the responder's identity reveal, closing
///   the online brute-force budget-bypass. Wire-incompatible with 0.3.1.
pub const CLUSTERCUT_PROTOCOL_VERSION: &str = "0.3.3";
```

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors. Existing tests still compile (no behaviour change yet).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/discovery.rs
git commit -m "Bump CLUSTERCUT_PROTOCOL_VERSION to 0.3.3 (wire break)"
```

---

## Task 2: Add `INITIATOR_KC_PLAINTEXT` constant in `crypto.rs`

**Files:**
- Modify: [src-tauri/src/crypto.rs:53-57](src-tauri/src/crypto.rs#L53-L57) — the existing block of pairing-protocol constants.

The KC plaintext is a fixed byte string. The AEAD tag computed over `(k_i2r, nonce, KC_PLAINTEXT)` is what authenticates the initiator's key derivation; the plaintext itself carries no information. A versioned label binds the constant to 0.3.3 so a future revision can't accidentally accept the older form.

- [ ] **Step 1: Add the constant**

Insert immediately after the existing `HKDF_INFO_R2I` line (around line 57):

```rust
/// Fixed plaintext for the T2 `InitiatorKC` AEAD frame (wire 0.3.3). The
/// initiator encrypts this byte string under `k_i2r`; the responder
/// decrypts under its own `k_i2r` and treats a tag failure as a wrong-PIN
/// or active-MITM event (`record_pairing_aead_failure`). The plaintext
/// itself is not a secret — the security comes from the AEAD tag binding
/// the (sub-key, nonce, plaintext) triple.
pub const INITIATOR_KC_PLAINTEXT: &[u8] = b"clustercut-pair-v2-init-kc";
```

- [ ] **Step 2: Add a happy-path round-trip test**

Add inside the existing `#[cfg(test)] mod tests { ... }` block, after the `pair_aead_round_trip` test (around line 198):

```rust
#[test]
fn initiator_kc_round_trip_with_matching_pin() {
    let (msg_i, msg_r, k_init, k_resp) = run_spake2_pair("hello", "hello");
    assert_eq!(k_init, k_resp);
    let t = pairing_transcript(&msg_i, &msg_r);
    let (k_i2r_init, _) = derive_pair_subkeys(&k_init, &t).unwrap();
    let (k_i2r_resp, _) = derive_pair_subkeys(&k_resp, &t).unwrap();

    let nonce = fresh_pair_nonce();
    let ct = pair_aead_encrypt(&k_i2r_init, &nonce, INITIATOR_KC_PLAINTEXT).unwrap();
    let pt = pair_aead_decrypt(&k_i2r_resp, &nonce, &ct).unwrap();
    assert_eq!(pt, INITIATOR_KC_PLAINTEXT);
}
```

- [ ] **Step 3: Add a wrong-PIN regression test**

Add immediately after the test from Step 2:

```rust
#[test]
fn initiator_kc_fails_closed_under_wrong_pin() {
    // The whole point of T2 InitiatorKC: a wrong-PIN attacker must not be
    // able to produce a ciphertext the responder will accept. Decrypt MUST
    // fail before the responder reveals its T3 ResponderId.
    let (msg_i, msg_r, k_init, k_resp) = run_spake2_pair("attacker-pin", "real-pin");
    let t = pairing_transcript(&msg_i, &msg_r);
    let (k_i2r_init, _) = derive_pair_subkeys(&k_init, &t).unwrap();
    let (k_i2r_resp, _) = derive_pair_subkeys(&k_resp, &t).unwrap();
    assert_ne!(k_i2r_init, k_i2r_resp);

    let nonce = fresh_pair_nonce();
    let ct = pair_aead_encrypt(&k_i2r_init, &nonce, INITIATOR_KC_PLAINTEXT).unwrap();
    let result = pair_aead_decrypt(&k_i2r_resp, &nonce, &ct);
    assert!(result.is_err(), "wrong-PIN InitiatorKC must AEAD-fail closed");
}
```

- [ ] **Step 4: Run the new tests and confirm they pass**

Run: `cd src-tauri && cargo test --lib crypto::tests::initiator_kc 2>&1 | tail -20`
Expected: `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/crypto.rs
git commit -m "Add INITIATOR_KC_PLAINTEXT constant and KC round-trip tests"
```

---

## Task 3: Add `InitiatorKC` variant to `PairingMessage`

**Files:**
- Modify: [src-tauri/src/protocol.rs:324-356](src-tauri/src/protocol.rs#L324-L356) — the doc block and the enum.

The enum is JSON-tagged (externally tagged, serde default). The new variant carries the same `{ nonce, ciphertext }` shape as the existing two AEAD variants, so no helper changes are required in `transport.rs` (read_pairing_frame / write_pairing_frame deserialise the whole enum).

- [ ] **Step 1: Update the doc block**

Replace lines 324–345 (the long doc comment above the enum) with:

```rust
/// Messages exchanged on the dedicated plaintext-TCP pairing channel.
/// Pairing never travels over QUIC: SPAKE2 needs no transport-layer
/// confidentiality (it's a PAKE), and wrapping it in unauthenticated TLS
/// adds complexity without security. Once pairing completes, all further
/// traffic moves to mutually-authenticated QUIC (steady-state `Message`).
///
/// Wire-protocol 0.3.3 (this file is the sole spec):
///
/// ```text
/// T0  Initiator → Responder   PairRequest  { spake_msg }
/// T1  Responder → Initiator   PairResponse { spake_msg }
/// T2  Initiator → Responder   InitiatorKC  { nonce, ciphertext = AEAD(k_i2r, nonce, INITIATOR_KC_PLAINTEXT) }
/// T3  Responder → Initiator   ResponderId  { nonce, ciphertext = AEAD(k_r2i, nonce, inner) }
/// T4  Initiator → Responder   InitiatorId  { nonce, ciphertext = AEAD(k_i2r, nonce, inner) }
/// ```
///
/// 0.3.3 diff from 0.3.1: T2 `InitiatorKC` is new. It forces the initiator
/// to prove possession of the SPAKE2-derived `k_i2r` (i.e. correct PIN)
/// *before* the responder sends its AEAD-encrypted identity. This closes
/// the online brute-force budget-bypass where a 0.3.1 attacker could
/// disconnect after T2 and brute-force the captured ResponderId offline
/// without ever triggering an AEAD-failure counter increment.
///
/// No plaintext identity fields on the pairing channel: `device_id` and
/// fingerprint appear only inside the AEAD-protected `inner` (see
/// [`PairIdInner`]). A wrong-PIN MITM derives a different `k_{i2r,r2i}`
/// and AEAD decryption fails closed before either side trusts a byte of
/// the payload. The cluster bootstrap state (`cluster_id`, `known_peers`,
/// `network_name`) is deferred to a post-pairing exchange over QUIC/mTLS
/// — see [`Message::ClusterInfoRequest`] / [`Message::ClusterInfo`].
```

- [ ] **Step 2: Insert the new enum variant**

Modify the `PairingMessage` enum (currently lines 346–356) — insert the new variant between `PairResponse` and `ResponderId`:

```rust
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PairingMessage {
    /// T0 — opening SPAKE2 element from the initiator. No identity bytes.
    PairRequest { spake_msg: Vec<u8> },
    /// T1 — answering SPAKE2 element from the responder. No identity bytes.
    PairResponse { spake_msg: Vec<u8> },
    /// T2 (wire 0.3.3) — initiator's AEAD-wrapped key-confirmation frame.
    /// Plaintext is the fixed `INITIATOR_KC_PLAINTEXT` constant; the tag
    /// authenticates the (k_i2r, nonce, plaintext) triple. Responder bumps
    /// the H1 AEAD-failure counter on tag failure.
    InitiatorKC { nonce: Vec<u8>, ciphertext: Vec<u8> },
    /// T3 — responder's AEAD-wrapped identity, decryptable under `k_r2i`.
    ResponderId { nonce: Vec<u8>, ciphertext: Vec<u8> },
    /// T4 — initiator's AEAD-wrapped identity, decryptable under `k_i2r`.
    InitiatorId { nonce: Vec<u8>, ciphertext: Vec<u8> },
}
```

- [ ] **Step 3: Update the existing serialise-round-trip test if needed**

Check the test at [src-tauri/src/protocol.rs:592](src-tauri/src/protocol.rs#L592). It uses `PairingMessage::PairRequest { spake_msg: vec![1, 2, 3] }`. That still compiles unchanged — no test changes required. Confirm by reading the block around line 590.

- [ ] **Step 4: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors.

This **will** break the existing pairing handlers because the responder still does `match { Ok(PairingMessage::InitiatorId { ... }) => ... }` at the T3 slot and the initiator still expects T2 to be `ResponderId`. We fix those in Tasks 4 and 5. Build should still pass; only behaviour is broken.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/protocol.rs
git commit -m "Add InitiatorKC variant to PairingMessage (wire 0.3.3)"
```

---

## Task 4: Responder — read `InitiatorKC` between SPAKE-finish and `ResponderId` send

**Files:**
- Modify: [src-tauri/src/lib.rs:2173-2211](src-tauri/src/lib.rs#L2173-L2211) — the responder block in `handle_pairing_connection` that currently sends `ResponderId` immediately after `derive_pair_subkeys` returns.

We insert a read+decrypt step *before* the existing T2 (now T3) send. Two failure modes get distinct treatment:
- **AEAD tag failure / nonce-length failure** → `record_pairing_aead_failure` (counts toward H1 lockout). This is the wrong-PIN / active-MITM path.
- **Wrong-variant frame** (e.g. an old 0.3.1 initiator sending `InitiatorId` instead of `InitiatorKC`) → `log_pairing_failure` (no counter). This is an honest version-mismatch; we don't want it to lock the listener out.

- [ ] **Step 1: Insert the InitiatorKC read between SPAKE2-finish and the renumbered T3 send**

Currently the responder flow is (around lines 2148–2211):

```rust
    // Finish SPAKE2 → shared 32-byte session key.
    let session_key = match crypto::finish_spake2(spake_state, &spake_msg_i) { ... };
    if session_key.len() != 32 { ... }
    let transcript = crypto::pairing_transcript(&spake_msg_i, &spake_msg_r);
    let (k_i2r, k_r2i) = match crypto::derive_pair_subkeys(&session_key, &transcript) { ... };
    tracing::info!("SPAKE2 complete (responder) for {}; sending ResponderId (T2).", peer_addr);

    // Refuse to advance to T2 if we have no cluster identity to bind to.
    // ...
    if state.cluster_id.lock().unwrap().is_empty() { ... }

    // T2 — responder's AEAD-wrapped identity, decryptable by the initiator
    // only if it derived the same SPAKE2 key (i.e. correct PIN).
    let r_inner = crate::protocol::PairIdInner { ... };
    ...
```

Insert the new T2 read between the `tracing::info!("SPAKE2 complete (responder) ...")` line and the `cluster_id.is_empty()` check. Replace the `tracing::info!` line and the cluster-id check with the block below, leaving the rest of the original block (the T3 send) unchanged:

```rust
    tracing::info!(
        "SPAKE2 complete (responder) for {}; awaiting InitiatorKC (T2).",
        peer_addr
    );

    // T2 (wire 0.3.3) — initiator's key-confirmation frame. Must AEAD-verify
    // under our k_i2r before we reveal any encrypted identity material.
    // A wrong-PIN attacker can't produce a tag the responder will accept, so
    // tag failures here count toward the H1 lockout exactly like a wrong T3
    // would have in 0.3.1.
    let (kc_nonce_vec, kc_ciphertext) = match crate::transport::read_pairing_frame(&mut stream).await {
        Ok(PairingMessage::InitiatorKC { nonce, ciphertext }) => (nonce, ciphertext),
        Ok(other) => {
            // Wrong variant — almost certainly a 0.3.1 client sending its old
            // `InitiatorId` at the T2 slot. Don't bump the AEAD counter, just
            // log + close. The pre-flight version check in start_pairing will
            // catch this for the initiator side; here we just hang up cleanly.
            log_pairing_failure(&state, peer_addr, &format!("expected InitiatorKC, got {:?}", other));
            return;
        }
        Err(e) => {
            log_pairing_failure(&state, peer_addr, &format!("read InitiatorKC failed: {}", e));
            return;
        }
    };
    let kc_nonce_arr: [u8; 12] = match kc_nonce_vec.as_slice().try_into() {
        Ok(arr) => arr,
        Err(_) => {
            record_pairing_aead_failure(&state, &app_handle, peer_addr, "InitiatorKC nonce length");
            return;
        }
    };
    match crypto::pair_aead_decrypt(&k_i2r, &kc_nonce_arr, &kc_ciphertext) {
        Ok(plaintext) => {
            // Defence in depth: also require the plaintext byte string match,
            // so a future variant of the wire that re-uses the InitiatorKC
            // shape can't be replayed against a 0.3.3 responder.
            if plaintext.as_slice() != crypto::INITIATOR_KC_PLAINTEXT {
                record_pairing_aead_failure(
                    &state,
                    &app_handle,
                    peer_addr,
                    "InitiatorKC plaintext mismatch",
                );
                return;
            }
        }
        Err(e) => {
            // The big one: wrong PIN or active MITM forging T2. Counter++.
            record_pairing_aead_failure(
                &state,
                &app_handle,
                peer_addr,
                &format!("InitiatorKC AEAD decrypt failed: {}", e),
            );
            return;
        }
    }
    tracing::info!(
        "InitiatorKC verified for {}; sending ResponderId (T3).",
        peer_addr
    );

    // Refuse to advance to T3 if we have no cluster identity to bind to.
    // Responding here would leak a valid ResponderId for a half-built
    // cluster; better to abort early.
    if state.cluster_id.lock().unwrap().is_empty() {
        log_pairing_failure(&state, peer_addr, "responder has no cluster_id");
        return;
    }
```

- [ ] **Step 2: Update the existing `T2 — responder's AEAD-wrapped identity` comment to T3**

Around line 2183, the comment currently reads:

```rust
    // T2 — responder's AEAD-wrapped identity, decryptable by the initiator
    // only if it derived the same SPAKE2 key (i.e. correct PIN).
```

Replace with:

```rust
    // T3 (wire 0.3.3) — responder's AEAD-wrapped identity, decryptable by
    // the initiator only if it derived the same SPAKE2 key (i.e. correct
    // PIN). Sent only after T2 InitiatorKC has been verified.
```

- [ ] **Step 3: Update the existing `T3 — initiator's AEAD-wrapped identity` to T4**

Around line 2213, the comment currently reads:

```rust
    // T3 — initiator's AEAD-wrapped identity.
```

Replace with:

```rust
    // T4 (wire 0.3.3) — initiator's AEAD-wrapped identity.
```

- [ ] **Step 4: Update the `T4 — drop the stream` end comment to T5**

Around line 2312, the comment currently reads:

```rust
    // T4 — drop the stream. The kernel closes the TCP connection, which
    // the initiator reads as the "responder is ready for QUIC" signal.
```

Replace with:

```rust
    // T5 (wire 0.3.3) — drop the stream. The kernel closes the TCP
    // connection, which the initiator reads as the "responder is ready
    // for QUIC" signal.
```

- [ ] **Step 5: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors. Initiator still sends old T3 (`InitiatorId`) so two 0.3.3 builds talking to each other still fail — Task 5 fixes the initiator.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "Responder: require InitiatorKC at T2 before sending ResponderId (wire 0.3.3)"
```

---

## Task 5: Initiator — send `InitiatorKC` between SPAKE-finish and `ResponderId` read

**Files:**
- Modify: [src-tauri/src/lib.rs:1843-1907](src-tauri/src/lib.rs#L1843-L1907) — the initiator block in `start_pairing` between `derive_pair_subkeys` and the `ResponderId` read.

- [ ] **Step 1: Insert the T2 `InitiatorKC` send between sub-key derivation and the `ResponderId` read**

Currently the initiator flow is (around lines 1843–1864):

```rust
    let transcript = crypto::pairing_transcript(&spake_msg_i, &spake_msg_r);
    let (k_i2r, k_r2i) = crypto::derive_pair_subkeys(&session_key, &transcript)
        .map_err(|e| format!("HKDF sub-key derivation failed: {}", e))?;
    tracing::info!("SPAKE2 complete (initiator); awaiting ResponderId (T2).");

    // T2 — responder's AEAD-wrapped identity (device_id + cert fingerprint).
    let (nonce_r, ciphertext_r) = match crate::transport::read_pairing_frame(&mut stream).await {
        Ok(PairingMessage::ResponderId { nonce, ciphertext }) => (nonce, ciphertext),
        ...
```

Replace the `tracing::info!` line and add the new T2 send between it and the `ResponderId` read:

```rust
    let transcript = crypto::pairing_transcript(&spake_msg_i, &spake_msg_r);
    let (k_i2r, k_r2i) = crypto::derive_pair_subkeys(&session_key, &transcript)
        .map_err(|e| format!("HKDF sub-key derivation failed: {}", e))?;
    tracing::info!("SPAKE2 complete (initiator); sending InitiatorKC (T2).");

    // T2 (wire 0.3.3) — explicit key-confirmation under k_i2r. The responder
    // refuses to send T3 (ResponderId) until this AEAD-verifies. Encrypting
    // the fixed KC_PLAINTEXT here is what proves to the responder that we
    // derived the same SPAKE2 key (i.e. we have the right PIN); a wrong-PIN
    // attacker can't forge a tag that decrypts under the responder's k_i2r.
    let nonce_kc = crypto::fresh_pair_nonce();
    let ciphertext_kc = crypto::pair_aead_encrypt(
        &k_i2r,
        &nonce_kc,
        crypto::INITIATOR_KC_PLAINTEXT,
    )
    .map_err(|e| format!("InitiatorKC AEAD encrypt failed: {}", e))?;
    let t2 = PairingMessage::InitiatorKC {
        nonce: nonce_kc.to_vec(),
        ciphertext: ciphertext_kc,
    };
    if let Err(e) = crate::transport::write_pairing_frame(&mut stream, &t2).await {
        let _ = app_handle.emit("pairing-failed", "Pairing connection failed. Please try again.");
        return Err(format!("Failed to send InitiatorKC: {}", e));
    }
    tracing::info!("InitiatorKC sent (initiator); awaiting ResponderId (T3).");

    // T3 (wire 0.3.3) — responder's AEAD-wrapped identity (device_id + cert
    // fingerprint). Sent only after the responder verifies our T2 KC frame.
    let (nonce_r, ciphertext_r) = match crate::transport::read_pairing_frame(&mut stream).await {
        Ok(PairingMessage::ResponderId { nonce, ciphertext }) => (nonce, ciphertext),
```

(The rest of the `match` body and downstream code is unchanged.)

- [ ] **Step 2: Update the existing `T3 — initiator's AEAD-wrapped identity` comment to T4**

Around line 1888, the comment currently reads:

```rust
    // T3 — initiator's AEAD-wrapped identity. Build, encrypt, send.
```

Replace with:

```rust
    // T4 (wire 0.3.3) — initiator's AEAD-wrapped identity. Build, encrypt, send.
```

- [ ] **Step 3: Update the `T4 — wait for the responder` comment to T5**

Around line 1952, the comment currently reads:

```rust
    // T4 — wait for the responder to finish processing T3 (pinning our
    // fingerprint) and close its side of the TCP socket. Reading to EOF on
```

Replace with:

```rust
    // T5 (wire 0.3.3) — wait for the responder to finish processing T4
    // (pinning our fingerprint) and close its side of the TCP socket.
    // Reading to EOF on
```

- [ ] **Step 4: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors.

- [ ] **Step 5: Run the existing unit tests**

Run: `cd src-tauri && cargo test --lib crypto 2>&1 | tail -30`
Expected: all crypto tests pass, including the two new `initiator_kc_*` ones from Task 2.

- [ ] **Step 6: Manual end-to-end check (two 0.3.3 builds)**

Run: `npm run tauri dev` on two machines (or two builds on the same machine using different config dirs). Pair them with the correct PIN. Pairing should succeed end-to-end, the per-peer fingerprint should pin, and the post-pairing QUIC `ClusterInfo` exchange should complete. Then deliberately mis-type the PIN on the second machine 10 times — confirm the H1 lockout trips and the "Pairing locked" notification fires (existing behaviour, but now driven by the new T2 KC failure path).

If you have a 0.3.1 build available, additionally confirm:
- 0.3.3 initiator → 0.3.1 responder: pairing fails with a generic message (the 0.3.1 responder is reading what it thinks is `InitiatorId` at the old T3 slot but gets `InitiatorKC`; it logs and closes).
- 0.3.1 initiator → 0.3.3 responder: pairing fails. 0.3.3 responder reads `InitiatorId` at T2, logs wrong-variant (no counter bump), closes.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "Initiator: send InitiatorKC at T2 before reading ResponderId (wire 0.3.3)"
```

---

## Task 6: Raise backend `is_protocol_compatible` floor to `(0, 3, 3)`

**Files:**
- Modify: [src-tauri/src/lib.rs:1080-1091](src-tauri/src/lib.rs#L1080-L1091)

The backend's compatibility floor gates the `peer-incompatible` modal fired from `report_send_failure`. After this task, any 0.3.1 peer surfaces as incompatible on the first user-triggered send failure.

- [ ] **Step 1: Update the function and its doc comment**

Replace the existing block (currently lines 1080–1091):

```rust
/// True if the peer's advertised `proto` version is at least the minimum
/// this build can talk to (currently 0.3.0 — the strict-mTLS line). Returns
/// false for peers that don't advertise the property at all (older builds).
pub fn is_protocol_compatible(version: Option<&str>) -> bool {
    let Some(v) = version else { return false };
    // 0.3.1 break: the pairing-channel wire format is wholly incompatible
    // with 0.3.0 (no plaintext device_id at T0/T1, AEAD-wrapped identity
    // at T2/T3, no Welcome — see WIRE-PROTOCOL-0.3.1.md). Bumping the
    // floor surfaces 0.3.0 peers as incompatible in the same UI flow we
    // built for the 0.2.x → 0.3.0 break.
    parse_protocol_version(v).map_or(false, |parsed| parsed >= (0, 3, 1))
}
```

with:

```rust
/// True if the peer's advertised `proto` version is at least the minimum
/// this build can talk to. Returns false for peers that don't advertise
/// the property at all (older builds).
pub fn is_protocol_compatible(version: Option<&str>) -> bool {
    let Some(v) = version else { return false };
    // 0.3.3 break: the pairing-channel wire format requires the new T2
    // `InitiatorKC` frame; 0.3.1 initiators don't send it and 0.3.1
    // responders don't read it. Bumping the floor surfaces 0.3.1 peers
    // as incompatible in the same UI flow used for the 0.2.x → 0.3.0
    // and 0.3.0 → 0.3.1 breaks.
    parse_protocol_version(v).map_or(false, |parsed| parsed >= (0, 3, 3))
}
```

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "Raise is_protocol_compatible floor to 0.3.3"
```

---

## Task 7: Raise frontend `MIN_COMPATIBLE_PROTOCOL` to `[0, 3, 3]`

**Files:**
- Modify: [src/App.tsx:51](src/App.tsx#L51)

The frontend `MIN_COMPATIBLE_PROTOCOL` constant is currently `[0, 3, 0]` — i.e. it has not been bumped since the original 0.2.x → 0.3.0 break. That means since 0.3.1 shipped, the per-peer amber-triangle indicator and the nearby-cluster per-device indicator have *not* been firing for 0.3.0 peers (only the modal, fired off backend `peer-incompatible` events on send failure). Fixing this in lockstep with the 0.3.3 bump catches both delinquencies at once.

- [ ] **Step 1: Update the constant**

Replace line 51:

```tsx
const MIN_COMPATIBLE_PROTOCOL: [number, number, number] = [0, 3, 0];
```

with:

```tsx
const MIN_COMPATIBLE_PROTOCOL: [number, number, number] = [0, 3, 3];
```

- [ ] **Step 2: Typecheck**

Run: `npx tsc --noEmit 2>&1 | head -20`
Expected: no output (clean typecheck).

- [ ] **Step 3: Manual UI verification**

If you have a 0.3.1 (or earlier) peer on the LAN, confirm in the running 0.3.3 build that:
1. The 0.3.1 peer appears in the trusted-peers list with the amber `AlertTriangle` next to its hostname (the title attribute reads "is running an older version of ClusterCut and won't be able to send or receive clipboard data. Please upgrade it.").
2. The 0.3.1 peer appears under "Nearby Clusters" with the same amber triangle next to its device entry.
3. Triggering a clipboard send to it still pops the "Peer needs updating" modal (via the backend's `report_send_failure`).

- [ ] **Step 4: Commit**

```bash
git add src/App.tsx
git commit -m "Raise frontend MIN_COMPATIBLE_PROTOCOL to 0.3.3"
```

---

## Task 8: Pre-flight version check in `start_pairing`

**Files:**
- Modify: [src-tauri/src/lib.rs:1764-1798](src-tauri/src/lib.rs#L1764-L1798) — the top of `start_pairing` where `peer_addr` is resolved.

Right now `start_pairing` opens a TCP socket regardless of the peer's advertised proto and fails at the wire level if the peer is incompatible (the initiator's read of `ResponderId` returns EOF, surfacing as "Pairing session expired"). For mDNS-discovered peers we have the proto string in the runtime `Peer` record before we ever connect — we can refuse early and emit `peer-incompatible` so the existing modal fires, giving a clearer error than the timeout path.

For the manual `Add Remote` path (`peer_addr.is_some()`) we have no mDNS data, so we still fall through to the wire-level failure with the existing generic error message. That's acceptable: the user explicitly typed an address and the wire-level failure is reasonably actionable.

- [ ] **Step 1: Insert the pre-flight version check immediately after peer-address resolution**

The current resolution block (lines 1782–1798) ends like this:

```rust
    let is_manual_pair = peer_addr.is_some();
    let peer_addr = if let Some(addr_str) = peer_addr {
        if let Ok(sock) = addr_str.parse::<std::net::SocketAddr>() {
            sock
        } else if let Ok(ip) = addr_str.parse::<std::net::IpAddr>() {
            std::net::SocketAddr::new(ip, 4654)
        } else {
            return Err(format!("Invalid peer address: {}", addr_str));
        }
    } else {
        let peers = state.get_peers();
        if let Some(peer) = peers.get(&peer_id) {
            std::net::SocketAddr::new(peer.ip, peer.port)
        } else {
            return Err("Peer not found".to_string());
        }
    };
```

Modify the discovered-peer branch (the `else` arm) to also pull the advertised proto and hostname, and reject early if below floor:

```rust
    let is_manual_pair = peer_addr.is_some();
    let (peer_addr, discovered_proto_version, discovered_hostname) = if let Some(addr_str) = peer_addr {
        let sock = if let Ok(sock) = addr_str.parse::<std::net::SocketAddr>() {
            sock
        } else if let Ok(ip) = addr_str.parse::<std::net::IpAddr>() {
            std::net::SocketAddr::new(ip, 4654)
        } else {
            return Err(format!("Invalid peer address: {}", addr_str));
        };
        // Add-Remote path: no mDNS data, so we can't pre-check the proto.
        // Fall through to the wire-level failure if the remote is incompatible.
        (sock, None, None)
    } else {
        let peers = state.get_peers();
        if let Some(peer) = peers.get(&peer_id) {
            (
                std::net::SocketAddr::new(peer.ip, peer.port),
                peer.protocol_version.clone(),
                Some(peer.hostname.clone()),
            )
        } else {
            return Err("Peer not found".to_string());
        }
    };

    // Pre-flight version check for mDNS-discovered peers. If the peer's
    // advertised proto is missing or below the floor this build can talk
    // to, emit `peer-incompatible` so the existing "Peer needs updating"
    // modal fires and abort before opening the TCP socket. For the manual
    // Add-Remote path (no discovered proto), we fall through and let the
    // wire-level failure handle it — the user explicitly typed the address
    // and we have no advance signal.
    if !is_manual_pair {
        if !is_protocol_compatible(discovered_proto_version.as_deref()) {
            let hostname = discovered_hostname.unwrap_or_else(|| peer_id.clone());
            tracing::warn!(
                "Refusing to pair with {} ({}): proto {:?} below floor.",
                hostname,
                peer_id,
                discovered_proto_version
            );
            let _ = app_handle.emit(
                "peer-incompatible",
                serde_json::json!({
                    "id": peer_id,
                    "hostname": hostname,
                }),
            );
            // Also surface to the pair-flow modal so the in-progress join
            // doesn't sit on a spinner forever waiting for ClusterInfo.
            let _ = app_handle.emit(
                "pairing-failed",
                format!("{} is running an older version of ClusterCut and can't pair with this device. Please upgrade it.", hostname),
            );
            return Err("Peer protocol version is below the minimum compatible floor.".to_string());
        }
    }
```

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: `Finished` with no errors. Note: this introduces a new use of `is_protocol_compatible` inside `start_pairing` — confirm it resolves (it's already `pub fn` in the same file at lib.rs:1083).

- [ ] **Step 3: Manual verification**

With a 0.3.1 (or earlier) peer on the LAN and the running 0.3.3 build:
1. In the trusted-peers list (where the peer is already paired), confirm the amber triangle is showing (Task 7 fix).
2. From the nearby-clusters list, click "Join" on a network whose peers advertise the older proto. Expected: the "Peer needs updating" modal pops with the older peer's hostname, AND the pair flow itself doesn't hang — the join modal closes / surfaces the error.
3. Use Add Remote with the IP of an older peer. Expected: pairing attempt still falls through to the wire-level failure with the generic "Pairing session expired" message. (We're deliberately not pre-checking this path.)

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "Pre-flight proto-version check in start_pairing for discovered peers"
```

---

## Task 9: Changelog entry

**Files:**
- Modify: [CHANGELOG.md](CHANGELOG.md) — extend the existing `## [Unreleased]` section.

Per [feedback_terse_changelog.md](../../../.claude/projects/-home-keith-LocalCode-keithvassallomt-ClusterCut/memory/feedback_terse_changelog.md), keep entries to 1–3 sentences.

- [ ] **Step 1: Add Security and Changed subsections under `## [Unreleased]`**

Add (preserving any existing `Added` / `Changed` / `Fixed` subsections already present):

```markdown
### Security
- Pairing channel: added an explicit key-confirmation packet (`InitiatorKC`) before the responder reveals its encrypted identity. Closes an online brute-force budget-bypass where an attacker could capture the responder's encrypted identity, disconnect without sending a fingerprint, and brute-force PINs offline without depleting the failure budget. Thanks @mdunphy for the writeup.

### Changed
- Wire protocol bumped to 0.3.3. Hard break — 0.3.1 peers will surface in the existing "please upgrade" UI flow. Frontend version-mismatch indicator now correctly reflects the backend floor (was stale at 0.3.0).
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "Changelog: wire 0.3.3 key-confirmation hardening"
```

---

## Self-Review

**Spec coverage:**
- Key-confirmation packet inserted before responder identity reveal — Tasks 2 (constant), 3 (enum), 4 (responder read), 5 (initiator send). ✓
- AEAD-fail counter bumped on wrong KC; wrong-variant frames do not count — Task 4 Step 1 explicitly splits the two failure modes. ✓
- Wire protocol version bumped end-to-end — Task 1 (advertised), Task 6 (backend floor), Task 7 (frontend floor). ✓
- Warning system: amber triangle correctly fires for sub-0.3.3 peers — Task 7 (fix stale frontend floor). ✓
- Warning system: pre-flight check refuses pairing against discovered incompatible peers and surfaces the existing modal — Task 8. ✓
- Add-Remote path is explicitly *not* pre-checked (no mDNS data) and falls through to the existing wire-level error — documented in Task 8 prose. ✓
- Changelog entry — Task 9. ✓

**Placeholder scan:** No TBD / TODO / "handle edge cases" / "similar to Task N" / unspecified types. ✓

**Type consistency:**
- `INITIATOR_KC_PLAINTEXT: &[u8]` (Task 2) used as the third arg to `pair_aead_encrypt`/`pair_aead_decrypt` and compared via `.as_slice()` against the decrypted plaintext (Tasks 4, 5) — matches the existing helper signatures.
- `PairingMessage::InitiatorKC { nonce: Vec<u8>, ciphertext: Vec<u8> }` matches the shape of the other AEAD variants exactly; `transport::write_pairing_frame` / `read_pairing_frame` need no changes.
- `discovered_proto_version: Option<String>`, `discovered_hostname: Option<String>` in Task 8 match the `Peer` struct's existing field types (peer.rs:34).

**Sequencing:** Tasks 1–3 are atomic prep (constant + variant). Task 4 (responder) and Task 5 (initiator) are the wire change proper; Task 4 alone breaks 0.3.1 initiators, Task 5 alone breaks 0.3.1 responders, both together complete the new flow. Tasks 6–7 raise the floors (UI-only, no wire effect). Task 8 layers the pre-flight on top. Task 9 closes out the changelog.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-27-wire-protocol-0.3.3.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using `executing-plans`, batch execution with checkpoints.

Which approach?
