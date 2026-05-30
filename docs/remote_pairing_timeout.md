# Remote Pairing Timeout (open, intermittent)

**Status:** open, root cause not yet captured. The symptom self-resolves before
a probe can witness it; we have hypotheses but no in-the-act evidence.

**Last observed:** 2026-05-30 by Keith, pairing Linux (Fedora 44, Tauri app) to
Windows peer at `192.168.96.7` over an OpenVPN tunnel (`vassallo_cloud`,
client source `10.8.0.8`).

## Symptom

When the user triggers "Add Remote Peer" with the remote's IP + PIN and the
remote is reachable over a VPN, the local Tauri app shows:

> Failed to connect to peer: pairing connect timeout

The behaviour is intermittent:

1. Sometimes the attempt times out as described.
2. Sometimes the local side times out but the *remote* (Windows) UI shows the
   peer as connected.
3. Sometimes the attempt simply succeeds.

A subsequent attempt may succeed without any intervening change.

## Error origin

The error string is emitted by [`pairing_connect`](../src-tauri/src/transport.rs#L648)
in [src-tauri/src/transport.rs:651-656](../src-tauri/src/transport.rs#L651-L656):

```rust
let stream = tokio::time::timeout(
    std::time::Duration::from_secs(10),
    TcpStream::connect(addr),
)
.await
.map_err(|_| "pairing connect timeout")??;
```

The wrapper is bypassed/preserved by the caller in
[src-tauri/src/lib.rs:1848-1850](../src-tauri/src/lib.rs#L1848-L1850):

```rust
let mut stream = crate::transport::pairing_connect(peer_addr)
    .await
    .map_err(|e| format!("Failed to connect to peer: {}", e))?;
```

So the user-visible error fires when, and only when, the local-side
`TcpStream::connect(addr)` does not produce an established socket within
10 seconds. The budget was sized for LAN — see the comment block at
[transport.rs:608-614](../src-tauri/src/transport.rs#L608-L614):

> "the wall-clock budget here covers TCP slowness + JSON encode/decode
>  + a stray re-transmit, not human input."

## Pairing protocol context

Pairing runs on plaintext TCP/4654. The exchange (per WIRE-PROTOCOL-0.3.3):

- **Initiator** opens TCP → `T0 PairRequest` → reads `T1 PairResponse` →
  `T2 InitiatorKC` → reads `T3 ResponderId` → `T4 InitiatorId` → close.
- **Responder** accepts TCP, gates on `pairing_accept_enabled` /
  `is_pairing_locked_out` / single-flight `pairing_slot` permit
  ([lib.rs:3382-3431](../src-tauri/src/lib.rs#L3382-L3431)),
  wraps `handle_pairing_connection` in a 10 s `PAIRING_PROTOCOL_TIMEOUT`,
  and only persists the peer to `known_peers.json` *after* T4 verifies
  ([lib.rs:2426-2432](../src-tauri/src/lib.rs#L2426-L2432)).

Post-pairing clipboard sync runs on a long-lived QUIC stream on the same
port — *separate* from the pairing TCP socket.

## Why the local-timeout / remote-connected asymmetry is not necessarily a bug

Two independent reasons the remote can show "connected" while the local TCP
connect times out, *without* the current attempt having reached T4:

1. **Stale persistence.** Once a pair has ever succeeded, the responder
   stores the peer in `known_peers.json` and leaves it there across failed
   subsequent attempts. The Windows UI keeps showing the peer.
2. **Live QUIC sync channel.** The clipboard-sync QUIC link is a separate
   socket from the pairing TCP socket. If a previous pairing succeeded,
   QUIC can be in steady state at the moment a new pairing attempt's TCP
   connect is failing.

So if the symptom is "I see the timeout, but remote shows me connected,"
that alone is not evidence of a bug — it is consistent with VPN flakiness
on the pairing TCP path only.

## Hypotheses

### H1: VPN TCP-connect latency exceeds 10 s on flaky tunnel — *most likely, unconfirmed*

Linux retransmits SYN with exponential backoff (~1 s, 2 s, 4 s, 8 s). If a
couple of SYNs are dropped during an idle-tunnel wake or rekey, the 10 s
budget runs out before the 3-way handshake completes. When the tunnel is
warm, the handshake fits in <100 ms (verified — see diagnostic run below).

**Fix if confirmed:** widen `pairing_connect`'s timeout (e.g. 25–30 s),
possibly only on the manual-Add-Remote path, and optionally add one SYN-
level retry.

### H2: Responder drops stream immediately after accept — *unlikely*

If the responder rejects via `pairing_accept_enabled = false`,
`is_pairing_locked_out()`, or `pairing_slot` exhaustion, the initiator
TCP connect succeeds (3-way handshake completes) but the *next* step
(write T0 / read T1) errors. That would surface as `"Failed to send
PairRequest"` or `"Failed to read PairResponse"`, not `"pairing connect
timeout"`. So this hypothesis does not match the observed string.

### H3: Race between tokio's 10 s timer and TCP establishment — *very unlikely*

The connect could in principle complete at the kernel exactly as tokio
fires the timer. But if `TcpStream::connect` had completed, `tokio::time::
timeout` would resolve to `Ok`. A drop after timeout would close the
socket; the remote would never see a fully established connection nor
any T0 byte, so no peer would be persisted in `known_peers.json`. So this
also does not explain a remote-connected outcome.

**Conclusion:** investigation should focus on H1.

## Diagnostic run, 2026-05-30

Performed while the tunnel was in the "right now it just works" state — i.e.
not during a failure.

```
$ time nc -zv -w 15 192.168.96.7 4654
Ncat: Connected to 192.168.96.7:4654.
... 5 attempts back-to-back, all OK in ~100 ms each.

$ ping -c 10 -W 2 192.168.96.7
10 packets transmitted, 0 received, 100% packet loss
```

ICMP being 100 % blocked while TCP/4654 succeeds is the normal
Windows-firewall posture — not a bug.

Route confirms the path is the VPN:

```
$ ip route get 192.168.96.7
192.168.96.7 dev vassallo_cloud table 52303 src 10.8.0.8 uid 1000
```

After tearing the VPN down, restarting the app on both ends, and bringing
the VPN back up, the symptom did **not** reproduce. We have no in-the-act
trace yet.

## Reproduction harness — `pair-probe.sh`

This is the probe script we use to capture the failure mode in evidence.
It originally lived at `tmp/pair-probe.sh` (ephemeral); reproduce it from
the listing below.

**How to use:**

1. `chmod +x pair-probe.sh && ./pair-probe.sh` (defaults to
   `192.168.96.7:4654`).
2. Trigger pairing in the app as usual.
3. When the UI shows "pairing connect timeout", note the wall-clock time.
4. Ctrl-C the probe and inspect the log line that matches that moment.

**Interpretation rubric:**

| Probe state around failure | Implication |
|---|---|
| All `OK`, elapsed < 100 ms | VPN was healthy. Look beyond the network: responder accept gating, app-side scheduling, blocked tokio runtime. |
| `FAIL` lines, or elapsed climbing toward 10 s | H1 confirmed. Widen `pairing_connect` budget and add a retry. |
| `OK` but elapsed 3–9 s | Tunnel is alive but slow. Same fix as H1. |

**Script (verbatim):**

```bash
#!/bin/bash
# Probes pairing TCP reachability to a remote peer while you try to pair.
# When the app reports "pairing connect timeout", the log around that wall-clock
# moment shows whether the VPN path was actually unreachable at that moment.
#
# Usage:
#   ./pair-probe.sh [IP] [PORT]
# Default: 192.168.96.7 4654
#
# Probe cadence: every 1s. Each probe has its own 11s budget (1s wider than the
# app's 10s, so we'd record success right before the app gave up if that were
# the boundary).

IP=${1:-192.168.96.7}
PORT=${2:-4654}
LOG="/tmp/pair-probe-$(date +%Y%m%d-%H%M%S).log"

echo "Probing ${IP}:${PORT} every 1s. Log: ${LOG}"
echo "Trigger pairing in the app now. Press Ctrl-C to stop."
echo ""
echo "ts                       result    elapsed   detail" | tee "${LOG}"
echo "------------------------ --------  --------  ---------------------------" | tee -a "${LOG}"

while true; do
    TS=$(date '+%Y-%m-%d %H:%M:%S.%3N')
    START=$(date +%s.%N)
    # nc -G is BSD; we have Ncat (Nmap). -w sets the timeout in seconds.
    OUT=$(nc -zv -w 11 "${IP}" "${PORT}" 2>&1)
    RC=$?
    END=$(date +%s.%N)
    ELAPSED=$(awk -v s="${START}" -v e="${END}" 'BEGIN { printf "%6.3fs", e - s }')

    if [ "${RC}" -eq 0 ]; then
        RESULT="OK      "
    else
        RESULT="FAIL    "
    fi

    # Strip newlines from ncat output for single-line log
    DETAIL=$(echo "${OUT}" | tr '\n' ' ' | sed 's/  */ /g' | head -c 80)

    echo "${TS}  ${RESULT}  ${ELAPSED}  ${DETAIL}" | tee -a "${LOG}"

    sleep 1
done
```

## Next steps when the symptom recurs

1. **Start `pair-probe.sh` before retrying.** Without an in-the-act probe
   trace, we are still guessing.
2. **Capture app-side logs.** The initiator has no per-step tracing around
   `pairing_connect`; if H1 is suspected, also note whether the symptom
   only follows a VPN-idle period (idle-tunnel wake) or whether it can
   appear mid-session.
3. **On the responder (Windows), check the tracing log for the matching
   accept.** If a TCP accept *did* occur at the same wall-clock moment as
   the initiator timeout, that rules H1 out (the SYN got through; the
   timeout was app-side) and forces us to re-open H2/H3.
4. **If H1 confirms**, the minimal change is to widen the budget in
   [transport.rs:652](../src-tauri/src/transport.rs#L652) and consider a
   single SYN-level retry. The LAN justification in the existing comment
   no longer holds for the manual-Add-Remote path, which is by design a
   cross-network entry point.

## Code references

- Timeout site: [src-tauri/src/transport.rs:648-658](../src-tauri/src/transport.rs#L648-L658)
- LAN-budget rationale: [src-tauri/src/transport.rs:608-614](../src-tauri/src/transport.rs#L608-L614)
- Caller / user-visible error: [src-tauri/src/lib.rs:1848-1850](../src-tauri/src/lib.rs#L1848-L1850)
- Responder accept gating: [src-tauri/src/lib.rs:3382-3431](../src-tauri/src/lib.rs#L3382-L3431)
- Responder peer persistence (post-T4): [src-tauri/src/lib.rs:2426-2432](../src-tauri/src/lib.rs#L2426-L2432)
- Responder handler entry: [src-tauri/src/lib.rs:2170](../src-tauri/src/lib.rs#L2170)
