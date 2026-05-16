# Wire Protocol 0.3.1 — Pairing-channel hardening

Working document distilling the security review thread with Michael, capturing the v0.3.1 wire-protocol changes on the dedicated plaintext-TCP pairing channel. Reflects Michael's final stamp of approval (round 4) and is the plan implementation should follow. Nothing in steady-state QUIC/mTLS changes.

## Point 1 — Fingerprints must be exchanged authenticated under the SPAKE2 key

### Michael's original concern

> The fingerprints must be securely exchanged (ie encrypted) using the spake2 session key, not plaintext — we're not under mutual QUIC yet.

The pairing flow in 0.3.0 exchanges cert fingerprints as plaintext typed-struct fields, with a separate `confirm = AEAD-Encrypt(K, FIXED_SENTINEL)` field. The confirm proves the sender knows K but does not authenticate the surrounding plaintext. An active MITM with no knowledge of K can forward both SPAKE2 messages, forward the fixed-sentinel confirm tag, and rewrite the plaintext fingerprint to one whose private key they hold — both peers then pin the attacker's fingerprint and the subsequent mTLS handshake succeeds against the wrong party.

### Proposed plan

Drop the separate confirm field. Each pairing frame becomes a single AEAD ciphertext over the entire serialised inner struct:

```
PairingFrame.payload = AEAD-Encrypt(k_direction, nonce, serialise(inner))
```

`inner` carries the fingerprint and the minimum binding fields needed (device_id, protocol-version tag). Successful decryption proves the sender held `k_direction` (which requires K, which requires the PIN) *and* that the bytes were not modified in flight. Decryption failure aborts pairing. No plaintext field on the pairing channel after SPAKE2 completes — only the two SPAKE2 element exchanges, which are designed to be public.

### Michael's follow-up changes

None. Approved as-is.

## Point 2 — Confirmation must bind the SPAKE2 transcript

### Michael's original concern

> The fingerprint exchange needs to include a hash of the pairing transcript as to give replay resistance.

`PAIR_CONFIRM_PLAINTEXT` was a single fixed constant used in both directions. Two problems: the construction was symmetric (one bug away from being reflectable), and nothing in the confirm artefact bound it to *this* SPAKE2 run.

### Proposed plan

Transcript hash + direction-distinct AEAD sub-keys, derived via HKDF.

```
k_i2r = HKDF-Expand(K, "clustercut-pair-v1 i2r" || transcript_hash, 32)
k_r2i = HKDF-Expand(K, "clustercut-pair-v1 r2i" || transcript_hash, 32)
```

The transcript hash never travels on the wire — both sides reconstruct it independently from the SPAKE2 bytes they already observed. Mismatch on either side → derived sub-keys diverge → AEAD decryption fails closed. Reflection and cross-run replay are both prevented.

### Michael's follow-up changes

The transcript construction changes. The PDF proposed a lexicographic sort of the two SPAKE2 messages (motivated by SPAKE2's algebraic symmetry). Michael's reply notes that lex sorting isn't required, because each side already knows its own role. Role-labelled concatenation is cleaner:

```
transcript = SHA-256("clustercut-pair-v1" || "initiator" || spake_msg_i || "responder" || spake_msg_r)
```

The fixed `"initiator"` / `"responder"` strings act as both ordering anchors and label separators.

## Point 3 — Defer Welcome contents to post-pairing QUIC

### Michael's original concern

> Can sending the welcome package be deferred until we're reconnected under QUIC? That keeps the SPAKE2 part short and laser focused and easy to audit/check: do the pairing, exchange fingerprints securely, end connection.

`WelcomePayload` ships `cluster_id`, `known_peers`, and `network_name` on the bespoke pairing channel alongside the responder's fingerprint. None of those fields are authentication material — they're cluster state, and they only need to come from a peer the initiator has already decided to trust.

### Proposed plan

Shrink the pairing channel to one job: bootstrap mutually pinned cert fingerprints. After both AEAD frames are exchanged and both sides pin, both close the TCP socket. Cluster state moves to a normal post-pairing QUIC exchange:

1. Initiator opens QUIC, mTLS succeeds against the freshly-pinned fingerprints.
2. Initiator → Responder: `ClusterInfoRequest` (over QUIC/mTLS).
3. Responder → Initiator: `ClusterInfo { cluster_id, known_peers, network_name }` (over QUIC/mTLS).
4. Initiator joins; normal `PeerDiscovery` gossip handles the rest.

Costs one extra round trip on a once-per-device-lifetime operation.

### Michael's follow-up changes

None. Approved as-is.

## Resulting protocol (end-to-end, with all of Michael's edits applied)

```
T0  Initiator → Responder   PairRequest  { spake_msg_i }
T1  Responder → Initiator   PairResponse { spake_msg_r }

    Both sides:
        K          = SPAKE2.finish(spake_msg_other)
        transcript = SHA-256("clustercut-pair-v1"
                             || "initiator" || spake_msg_i
                             || "responder" || spake_msg_r)
        k_i2r      = HKDF-Expand(K, "clustercut-pair-v1 i2r" || transcript, 32)
        k_r2i      = HKDF-Expand(K, "clustercut-pair-v1 r2i" || transcript, 32)

T2  Responder → Initiator   ResponderId {
                                nonce,
                                AEAD-Encrypt(k_r2i, nonce,
                                    serialise({ device_id_r, fingerprint_r }))
                            }
    Initiator: decrypt with k_r2i. On success, pin fingerprint_r for device_id_r.
    On failure, abort.

T3  Initiator → Responder   InitiatorId {
                                nonce,
                                AEAD-Encrypt(k_i2r, nonce,
                                    serialise({ device_id_i, fingerprint_i }))
                            }
    Responder: decrypt with k_i2r. On success, pin fingerprint_i for device_id_i.
    On failure, abort.

T4  Both close TCP. Pairing is complete.

T5  Initiator opens QUIC to Responder (mTLS, verifies against pinned fingerprints).
T6  Initiator → Responder   ClusterInfoRequest                 (over QUIC/mTLS)
T7  Responder → Initiator   ClusterInfo {                       (over QUIC/mTLS)
                                cluster_id, known_peers, network_name }
T8  Normal PeerDiscovery gossip.
```

Note: `device_id` is no longer in T0/T1 — it appears only inside the AEAD ciphertexts at T2/T3. See hardening item H2.

## Hardening items (red-team pass)

These came from a self-imposed red-team pass on the plan above. Items where Michael's reply only confirmed the proposal are summarised briefly; items where he flagged a change are written out fully.

### H1. Rate-limiting the pairing channel

Background: the bespoke pairing TCP channel is plaintext-reachable by anyone on the LAN. SPAKE2 defeats *offline* PIN brute force, but not online guessing at network speed. With a short numeric PIN on coffee-shop Wi-Fi, the search space is uncomfortably small. Combined with gossip transitivity (one successful pairing → full cluster membership), this is the most realistic remaining attack vector.

We proposed a per-source-IP attempt counter, exponential back-off, and hard lockout after N AEAD-decrypt failures.

**Michael's change.** Drop the per-IP state machine. The responder maintains a single global counter of AEAD-decrypt failures at T2/T3 (TCP closes do not count). After **N = 10–20** failures aggregated across all source IPs, the responder shuts the pairing listener entirely and flips a visible UI switch from "ON" to "OFF". Every incoming pairing TCP connection is refused at accept() until the user manually re-arms. The lockout is responder-wide, not per-peer — there is no per-IP state at all.

When the lockout triggers, the responder also surfaces an *urgent* user-facing notification (banner / modal / OS-level notification — implementation detail open, but intent is unmistakable). A passive toggle change in a settings panel is not enough; the user must actively see "your responder just locked itself out of pairing, here's why, click to re-arm."

The trade-off is intentional: a LAN-local attacker who can reach the pairing port can burn N failures and force a manual re-arm. Accepted because (a) the alternative per-IP rate limiter was explicitly rejected as too complex, (b) the attack surface is bounded to the window where the pairing UI is open, and (c) the OFF state is highly user-visible via the urgent notification.

### H2. Plaintext `device_id` at T0/T1

We offered (a) include device_ids in the transcript hash so any rewrite diverges the sub-keys, or (b) drop the plaintext device_ids from T0/T1 entirely and rely only on the AEAD-protected ones at T2/T3.

**Michael's choice: option (b).** Drop the plaintext device_ids. Rationale: "device_id is not reliable information until AFTER the successful pairing." The UI binds to the AEAD-protected device_id, never to a plaintext one.

### H3. AEAD nonce on the wire

We proposed a fixed all-zero nonce, since each sub-key is single-use and encrypts a single message — provably safe under ChaCha20-Poly1305 in that regime.

**Michael's change.** Keep the random nonce on the wire. Not strictly required given single-use sub-keys, but it is standard practice and there is no reason to weaken the construction below the standard belt-and-braces shape.

### H4. Max frame size

We proposed a ~1 KB cap with headroom for future fields.

**Michael's change (round 3).** Cap at *exactly* the expected number of bytes (the PDF estimated ~80 bytes for the current inner struct; the exact value is to be verified during implementation). If the wire format ever expands or shrinks, the cap moves with it. No forward-compatibility headroom — we are explicitly not aiming for a backwards-compatible pairing protocol.

**Michael's follow-up (round 4).** `device_id` is the one variable-length component of the inner struct. Cap `device_id` at a reasonable maximum (**256 bytes**); any `device_id` exceeding the cap is truncated before serialisation on both sides. The wire-frame size cap is then computed against the 256-byte `device_id` ceiling, so the exact byte cap is constant once the other fields' sizes are nailed down at implementation time. Truncation must be deterministic so both ends derive the same identifier from the same source `device_id`.

### H5. HKDF info-string canonicalisation

We proposed length-prefixing or delimiter bytes for domain separation hygiene.

**Michael's verdict: skip.** The fixed-length components are unambiguous today. Future rewrites would have to consider it if they alter the label or hash function. Not worth the cost now.

### H6. Single-flight pairing channel on the responder

We proposed accepting exactly one in-flight pairing TCP connection while the UI is open.

**Michael's response (round 3).** Fine for his 2-peer use case. He suggested a cap of 2 if we expect larger clusters.

The cap governs *concurrent in-flight pairing TCP connections on the responder* — not cluster size. The two concepts are independent.

Fan-out on join: when a new device joins a cluster of *any* size, exactly one SPAKE2 pairing happens — between the new device and the single existing peer the user pointed it at via PIN. The remaining peers in the cluster learn the new device's pinned cert fingerprint via PeerDiscovery gossip from the responder over their already-established mTLS/QUIC channels. No additional SPAKE2 handshakes occur with the other peers. Joining a 6-device cluster = 1 pairing operation; joining a 60-device cluster = 1 pairing operation.

**Michael's final decision (round 4): cap = 1 concurrent pairing.** Drop the cap=2 safety margin — the extra concurrency isn't worth its complexity, and a network attacker capable of DoSing the pairing port is not in our threat model. Any second pairing TCP connection is refused at accept() until the in-flight one completes.

**Connection-hold timeout (new in round 4).** With cap = 1, a single open-but-idle TCP connection blocks all other pairing attempts indefinitely. The responder MUST guard against this. Two acceptable shapes:

1. **Server-side idle timeout.** The responder closes any accepted pairing TCP connection that does not progress to a successful T2/T3 AEAD decrypt within a short bounded window (target: a few seconds — exact value to be picked during implementation based on observed round-trip + PIN-entry timing). Time starts at accept().
2. **Defer-connect on the initiator.** The initiator does not open the TCP socket until the user has entered the PIN and pressed OK, so the connection's lifetime on the wire is dominated by the SPAKE2 + AEAD round trips rather than by human PIN-entry time.

Both ends should adopt both shapes where practical — defer-connect on the initiator avoids holding our own slot during PIN entry, and a server-side timeout protects against misbehaving or malicious initiators that *do* hold the connection open. The server-side timeout is the load-bearing one for safety; the initiator-side defer is a UX/hygiene improvement.

### H7. Log-channel leak on PIN-mismatch failures

We proposed collapsing all pairing failures to a single generic message at INFO / user-facing level, with detailed errors only at DEBUG/TRACE.

**Michael's change.** Wrap this behind a debug-mode switch. When debug mode is off, the minimal generic message is logged. When debug mode is on, verbose details are written. Same security property, controlled by an explicit user-facing toggle rather than by log level alone.

## Compatibility note

This is a second wire-format break on the pairing channel within the 0.3.x cycle (the first was 0.2.x → 0.3.0 moving the pairing channel off QUIC onto plain TCP). The advertised mDNS proto version bumps once more in 0.3.1; peers below the new version get the same "please upgrade" treatment 0.3.0 already added for 0.2.x peers.

## Verification commitments

Carried over from the round-3 response, unchanged:

- **Adversarial pass in the PR description.** For each cryptographic artefact on the pairing channel, document what bytes it authenticates, what an attacker controlling only the TCP byte stream (no K) can substitute, and what an attacker holding a copy of one peer's public cert can substitute. If any answer is uncomfortable, the design changes before merge.
- **MITM unit test.** A test harness that interposes on the pairing TCP socket and rewrites AEAD ciphertext — pairing must abort on the receiving side with a decryption failure, not silently succeed.
- **Wrong-PIN test.** Mismatched PINs → SPAKE2 produces different Ks → sub-key HKDF diverges → AEAD fails → pairing aborts before any pinning happens.
- **Transcript-tamper test.** Inject a modified SPAKE2 message in flight; receiving side reconstructs a different transcript, derives different sub-keys, AEAD decryption fails closed.
- **Direction-swap test.** Replay an initiator-direction ciphertext as a responder-direction frame — must reject (sub-keys differ).
- **Cluster-info-over-QUIC test.** Confirm `ClusterInfo` only ever travels over mTLS-authenticated QUIC, never over the pairing TCP channel.

## Open items before implementation begins

1. **Exact `inner` byte count for H4.** Confirm the serialised size of `{ device_id (≤256B), fingerprint }` (plus any protocol-version tag) so the wire-frame cap can be pinned to a single constant.
2. **Connection-hold timeout value for H6.** Pick the server-side idle-timeout duration (seconds-scale) based on realistic round-trip + processing time once the implementation is in place.
