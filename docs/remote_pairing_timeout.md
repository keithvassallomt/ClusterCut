# Remote Pairing — PIN-mismatch failure with misleading error (FIX CANDIDATES IN FLIGHT, 2026-05-30)

**Status:** root cause partially identified. Confirmed: the failure is a
T2 AEAD mismatch (initiator's `k_i2r` ≠ responder's `k_i2r`), which can
only happen if the PIN bytes the two sides plug into SPAKE2 differ
(transcript divergence is the only other possibility and the network was
proven clean). Not confirmed yet: *why* the PINs diverged when the user
read the PIN fresh off Windows's UI and typed exactly that.

Three changes landed in this branch — two defensive against an
invisible-whitespace divergence (the most plausible mechanism I can
construct from the code), and one diagnostic to capture the actual
divergence next time. **None of these is yet proven to fix the bug.**
The next reproduction either succeeds (defensive fix was sufficient) or
the new debug log line nails the exact byte-level cause.

The user-reported symptom string ("Failed to connect to peer: pairing
connect timeout") was a red herring — that string only exists at the
TCP-connect-timeout site and was probably a stale UI banner from a
prior failure or a misread. The bug never had anything to do with TCP.

## What was actually happening

1. User triggers "Add Remote Peer" on Linux, types Windows's PIN, IP.
2. TCP connects fine (≈100 ms across the VPN; verified by probe).
3. T0 / T1 / SPAKE2 finish all succeed.
4. Initiator sends T2 (InitiatorKC) under `k_i2r`.
5. Responder's T2 AEAD decrypt fails — its derived `k_i2r` differs from
   the initiator's, which happens iff the PINs the two sides plugged
   into SPAKE2 differ (transcript divergence is the only other cause,
   and the network was proven clean).
6. Responder drops the stream ([lib.rs:2287-2294](../src-tauri/src/lib.rs#L2287-L2294)).
7. Initiator's T3 read gets EOF, hits the `Err` branch at the T3 read
   site, fires `pairing-failed` with the (previously) misleading
   *"Pairing session expired"* message.
8. User retries. PIN still mismatches. Same outcome. Loops.

The "what finally worked" sequence — Windows leaves cluster a second
time, generating a fresh PIN, Linux types the new PIN — was the user
re-reading the PIN after a UI refresh and happening to type one that
the responder agreed with.

`start_spake2` uses `Spake2::start_symmetric` with a fixed identity
([crypto.rs:13-24](../src-tauri/src/crypto.rs#L13-L24)) — the `_id_a`
and `_id_b` parameters are ignored — so identity-label mismatch can be
ruled out. Sub-key derivation
([crypto.rs:91-113](../src-tauri/src/crypto.rs#L91-L113)) depends only on
the SPAKE2 session key + transcript hash. Network was healthy, so
transcripts matched. That leaves the PIN as the sole independent
variable, and AEAD decryption fails closed on the smallest divergence.

## What changed in this branch

Three things. The first two are candidate fixes for the most plausible
mechanism; the third is instrumentation in case the first two miss.

### 1. Trim PIN on every backend save/load (`storage.rs`)

`load_network_pin` returns `pin.trim().to_string()` instead of the raw
file contents, and `save_network_pin` writes `pin.trim()` instead of
the raw input string. Belt-and-braces: even if some path writes
whitespace, the next load discards it; a legacy `network_pin` file with
trailing whitespace from any source gets healed on next read without
migration code.

### 2. Trim PIN in the Settings input (`App.tsx`)

The Add Remote Peer modal's PIN input already had `e.target.value.trim()`
in its `onChange`. The Settings/Provisioned-mode PIN input did not — so
a PIN entered there (or pasted with a trailing space/newline) would
land in `state.network_pin` with whitespace, get displayed in the UI
(whitespace invisible to the user), get read by the user, get typed
into the *other* device's Add-Remote modal which then *does* trim, and
the two sides plug different byte strings into SPAKE2. The Settings
input now matches the Add-Remote one.

### 3. Diagnostic logging in the responder's T2 AEAD-failure path (`lib.rs`)

Gated on the existing `pairing_debug_logs` flag (so it never leaks
PIN-bearing log lines in default config). When T2 AEAD decrypt fails,
the responder now emits `Responder PIN at T2-AEAD-failure: len=<N>
bytes=[<hex bytes>]`. Combined with the initiator-side trim boundary,
this is what diagnoses an invisible-whitespace or encoding divergence
*directly* instead of by elimination. If the trim fixes above don't
solve the bug, this log line tells us exactly what bytes the responder
plugged into SPAKE2 vs what the initiator must have sent.

### Also: the misleading error string

While in the area, also replaced the misleading *"Pairing session
expired. Please try again."* on the initiator's T3-read `Err` branch
with *"Failed to join network. The PIN may be incorrect."* — the same
string already used a few lines down for the symmetric (initiator-side
T3 AEAD decrypt) failure. This is a pure UX improvement, not the fix
for the underlying divergence, but it removes the specific phrasing
that misled the first investigation into a TCP-timeout hypothesis.

## Why this took a session and a half to find

The original symptom string the user reported — *"Failed to connect to
peer: pairing connect timeout"* — sits in
[lib.rs:1850-ish](../src-tauri/src/lib.rs) where `pairing_connect`'s 10 s
TCP-connect timeout is mapped. That string only appears for an actual
`TcpStream::connect` failure. Anchoring on it produced an entire
hypothesis tree (H1: VPN slowness, etc.) that the second-attempt
reproduction — with responder logs and a concurrent TCP probe — falsified
in one trace. Lessons worth keeping:

- **One-line user-reported error strings are not authoritative.** Verify
  the string actually exists in the source code along the failure path
  before building hypotheses around it. The reported string was probably
  a stale UI banner from a different earlier failure, or a misread of
  the toast.
- **The opaque `Pairing failed from <addr>.` line on the responder
  hides every protocol-step distinction.** Flipping `pairing_debug_logs`
  on (which the user did for the second attempt) made the specific
  AEAD-decrypt failure visible immediately. That toggle is the single
  best diagnostic in this subsystem — recommend it first, every time.
- **Per WIRE-PROTOCOL-0.3.1 §H7**, the *log* channel deliberately
  collapses failure modes to avoid leaking wrong-PIN vs. other-error
  distinctions to attackers. That principle was correctly applied to
  logs. It does NOT need to apply to user-facing error UX on the
  initiator side, which is the legitimate user's machine — there's no
  attacker to whom the message could leak anything they don't already
  know. Future error-message changes in this subsystem should follow
  the same split: opaque on the responder's log, specific on the
  initiator's UI.

## How to verify the candidate fixes

This is the procedure to run on the next reproduction. Two outcomes are
informative:

1. **Enable `pairing_debug_logs` on both sides** (Settings → toggle).
   The new diagnostic only fires under this flag, and we want it.
2. **On Windows, leave the cluster** (regenerates a clean PIN via the
   trimmed save/load path).
3. **On Linux, type Windows's freshly-displayed PIN** exactly as shown,
   no copy-paste from anywhere else.
4. **If pairing succeeds:** the trim fixes were sufficient. Bug was
   invisible whitespace getting into `state.network_pin` from some
   path that's now closed.
5. **If pairing still fails:** capture the responder's new
   `Responder PIN at T2-AEAD-failure: len=<N> bytes=[<hex bytes>]` log
   line. That gives us the exact byte sequence the responder used.
   Compare to what the initiator must have sent — if they differ, we
   have ground truth for the divergence and can write a targeted fix
   in one more cycle.

Side-effect verification (UX-only):

1. With everything paired correctly, deliberately type the *wrong* PIN
   in the Add Remote Peer modal.
2. Linux app should display *"Failed to join network. The PIN may be
   incorrect."* — not *"Pairing session expired"*. This is the
   misleading-message change, independent of whether the underlying
   divergence is solved.

## Source data preserved below

The two reproductions and the `pair-probe.sh` harness from the
investigation are kept verbatim at the foot of this document for future
reference if anything in this area regresses.

---

## Reproduction harness — `pair-probe.sh`

Originally written under the H1 (VPN-slowness) hypothesis. It still
works as a general-purpose pairing-port reachability prober and is
worth keeping in the repo for future investigations.

**How to use:**

1. `chmod +x pair-probe.sh && ./pair-probe.sh` (defaults to
   `192.168.96.7:4654`).
2. Trigger pairing in the app as usual.
3. When the UI shows the failure, note the wall-clock time.
4. Ctrl-C the probe and inspect the log line that matches that moment.

**Script (verbatim):**

```bash
#!/bin/bash
# Probes pairing TCP reachability to a remote peer while you try to pair.
# When the app reports a pairing failure, the log around that wall-clock
# moment shows whether the VPN path was actually unreachable at that moment.
#
# Usage:
#   ./pair-probe.sh [IP] [PORT]
# Default: 192.168.96.7 4654
#
# Probe cadence: every 1s. Each probe has its own 11s budget.

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
    OUT=$(nc -zv -w 11 "${IP}" "${PORT}" 2>&1)
    RC=$?
    END=$(date +%s.%N)
    ELAPSED=$(awk -v s="${START}" -v e="${END}" 'BEGIN { printf "%6.3fs", e - s }')

    if [ "${RC}" -eq 0 ]; then
        RESULT="OK      "
    else
        RESULT="FAIL    "
    fi

    DETAIL=$(echo "${OUT}" | tr '\n' ' ' | sed 's/  */ /g' | head -c 80)

    echo "${TS}  ${RESULT}  ${ELAPSED}  ${DETAIL}" | tee -a "${LOG}"

    sleep 1
done
```

## Raw evidence — first attempt (2026-05-30, TCP probe only)

Probe ran while tunnel was healthy. Every connect to `192.168.96.7:4654`
returned OK in ~100 ms — i.e. the network path was fine even when the
user was reporting "pairing connect timeout." This was the first hint
that the reported error string did not match what was actually
happening; it should have ended H1 sooner.

## Raw evidence — second attempt (2026-05-30, with responder logs)

Linux is `Europe/London` = BST = UTC+1; Tauri's tracing logs use UTC
(both sides). Subtract one hour from probe LOCAL timestamps to compare.

### Initiator-side probe log (LOCAL = BST)

```
./tmp/pair-probe.sh
Probing 192.168.96.7:4654 every 1s. Log: /tmp/pair-probe-20260530-070626.log
Trigger pairing in the app now. Press Ctrl-C to stop.

ts                       result    elapsed   detail
------------------------ --------  --------  ---------------------------
2026-05-30 07:06:26.534  OK         0.110s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:27.672  OK         0.091s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:28.790  OK         0.100s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:29.920  OK         0.084s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:31.022  OK         0.120s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:32.197  OK         0.105s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:33.347  OK         0.129s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:34.521  OK         0.119s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:35.695  OK         0.113s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:36.855  OK         0.095s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:38.014  OK         0.111s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:39.164  OK         0.161s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:40.380  OK         0.111s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:41.562  OK         0.097s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:42.696  OK         0.142s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:43.890  OK         0.121s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:45.087  OK         0.116s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:46.274  OK         0.138s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:47.447  OK         0.151s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:48.660  OK         0.130s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:49.854  OK         0.099s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:50.972  OK         0.092s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:52.084  OK         0.097s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:53.206  OK         0.135s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:54.379  OK         0.093s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:55.507  OK         0.120s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:56.658  OK         0.100s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:57.782  OK         0.104s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:06:58.928  OK         0.120s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:07:00.092  OK         0.116s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:07:01.263  OK         0.125s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
2026-05-30 07:07:02.438  OK         0.103s  Ncat: Version 7.92 ( https://nmap.org/ncat ) Ncat: Connected to 192.168.96.7:465
```

### Responder-side log (Windows, UTC, `pairing_debug_logs = false`)

```
2026-05-30T06:06:13.788730Z  WARN Pairing failed from 192.168.96.17:51508.
2026-05-30T06:06:20.752714Z  INFO [Discovery] Active probe FAILED/TIMEOUT. Removing peer clustercut-1100959039
2026-05-30T06:06:25.749101Z  WARN Pairing failed from 192.168.96.17:34032.
2026-05-30T06:06:26.865572Z  WARN Pairing failed from 192.168.96.17:34038.
2026-05-30T06:06:28.000840Z  WARN Pairing failed from 192.168.96.17:34046.
2026-05-30T06:06:29.103204Z  WARN Pairing failed from 192.168.96.17:34054.
2026-05-30T06:06:30.239182Z  WARN Pairing failed from 192.168.96.17:34066.
2026-05-30T06:06:31.395772Z  WARN Pairing failed from 192.168.96.17:59190.
2026-05-30T06:06:32.570162Z  WARN Pairing failed from 192.168.96.17:59202.
2026-05-30T06:06:33.735368Z  WARN Pairing failed from 192.168.96.17:59208.
2026-05-30T06:06:34.908076Z  WARN Pairing failed from 192.168.96.17:59222.
2026-05-30T06:06:36.044338Z  WARN Pairing failed from 192.168.96.17:59234.
2026-05-30T06:06:37.212881Z  WARN Pairing failed from 192.168.96.17:59246.
2026-05-30T06:06:38.418955Z  WARN Pairing failed from 192.168.96.17:59258.
2026-05-30T06:06:39.586995Z  WARN Pairing failed from 192.168.96.17:59272.
2026-05-30T06:06:40.769848Z  WARN Pairing failed from 192.168.96.17:59288.
2026-05-30T06:06:41.933510Z  WARN Pairing failed from 192.168.96.17:44270.
2026-05-30T06:06:43.107687Z  WARN Pairing failed from 192.168.96.17:44276.
2026-05-30T06:06:44.290134Z  WARN Pairing failed from 192.168.96.17:44282.
2026-05-30T06:06:45.503020Z  WARN Pairing failed from 192.168.96.17:44292.
2026-05-30T06:06:46.698631Z  WARN Pairing failed from 192.168.96.17:44302.
2026-05-30T06:06:47.878569Z  WARN Pairing failed from 192.168.96.17:44316.
2026-05-30T06:06:49.055267Z  WARN Pairing failed from 192.168.96.17:44330.
```

Source port appearing on the responder side is `192.168.96.17` (the
WireGuard server's IP, SNAT'ing the Linux client's `10.8.0.8`), not the
Linux tunnel IP. Expected for this topology; noted in case future
investigations are confused by the post-NAT source.

## Raw evidence — third attempt (2026-05-30, with `pairing_debug_logs = on`)

After the user enabled verbose pairing logs on the responder and
reproduced once more. This is the trace that nailed the diagnosis.

### Initiator (Linux)

```
2026-05-30T07:51:56.204433Z  INFO [Netmon] Re-probing 2 known peers
2026-05-30T07:51:57.262614Z  INFO Unregistering old service: clustercut-3699914280._clustercut._tcp.local.
2026-05-30T07:51:57.269477Z  INFO Registered service: clustercut-3699914280 (clustercut-3699914280._clustercut._tcp.local.) on 10.8.0.8:4654
2026-05-30T07:51:57.271107Z  INFO [Netmon] Re-registered mDNS service
2026-05-30T07:51:57.271488Z  INFO [Netmon] Re-probing 2 known peers
2026-05-30T07:52:30.662177Z  INFO SPAKE2 complete (initiator); sending InitiatorKC (T2).
2026-05-30T07:52:30.666404Z  INFO InitiatorKC sent (initiator); awaiting ResponderId (T3).
```

### Responder (Windows)

```
2026-05-30T07:51:34.375461Z ERROR Connection handshake failed from 192.168.96.17:4654: the cryptographic handshake failed: error 40: unexpected error: client cert fingerprint not in known peers: cdc6e775284f8f57
2026-05-30T07:52:30.181694Z  INFO Received PairRequest from 192.168.96.17:55808; running SPAKE2.
2026-05-30T07:52:30.212509Z  INFO SPAKE2 complete (responder) for 192.168.96.17:55808; awaiting InitiatorKC (T2).
2026-05-30T07:52:30.516927Z  WARN Pairing failed from 192.168.96.17:55808: InitiatorKC AEAD decrypt failed: pair AEAD decrypt failed: aead::Error
```

The QUIC handshake failure at 07:51:34 is unrelated — it's Linux trying
to maintain its old QUIC sync link to Windows after Windows had wiped
its `known_peers.json` by leaving the cluster, and Linux's cert
fingerprint is no longer trusted there. Visible-but-separate from the
pairing failure that follows.

The pairing trace itself is unambiguous: SPAKE2 completes on both
sides, the initiator successfully sends T2, the responder fails to
decrypt T2's AEAD tag → PIN mismatch.
