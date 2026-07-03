# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- Devices in a provisioned cluster now share one PIN as intended. Previously each device kept its own (often auto-generated) PIN, so a device that joined couldn't be paired with using the admin's cluster PIN. A device that joins a provisioned cluster now automatically switches to Provisioned mode and adopts the cluster's shared PIN (which it already entered to pair), so every device converges on the same value. The PIN is not sent over the network — the joiner reuses the one it typed.
- Fixed a pairing failure ("server cert fingerprint mismatch") that could stop a device from joining when the local peer list held more than one stale entry for the target's IP (e.g. leftover records from earlier test rounds). The mTLS client now accepts the peer's cert if it matches any fingerprint pinned for that address, instead of an arbitrary first match that could be a stale one.
- Joining a cluster now works with the PIN of any of its online devices. Previously "Join" tried a single, arbitrarily-chosen member, so the correct PIN was rejected unless it happened to belong to that one device. Join now tries the entered PIN against each online member until one accepts it.
- Leaving a cluster now reliably removes the departing device from the other members. The "I'm leaving" broadcast was sent as a background task and then immediately raced by the local reset that wiped the very keys needed to send it, so peers usually never got the message and kept the device pinned in their cluster (a stale entry that survived restarts and spawned "Unidentified" re-probe ghosts). The departure is now sent and confirmed before the reset. The same race is fixed for kicking a device.
- After a device leaves and re-appears under its new cluster name, already-running peers now see the new cluster without needing an app restart. The long-lived mDNS browse caches resolved services and wouldn't re-resolve a device that re-registered under a new cluster, so the new cluster stayed invisible until a restart (which starts a fresh browse). Now, when a peer is seen leaving, the app runs a fresh short-lived mDNS scan (empty cache, like a restart) that picks up the leaver's new cluster and refreshes the rest. A leaving device also takes a fresh device id so it re-appears as a brand-new service, and its mDNS de-registration is flushed before re-registering.

### Changed
- Device identifiers are now UUIDs, making it effectively impossible for two devices to independently generate the same id (the old 32-bit value could collide, especially on VMs provisioned from a shared template with correlated startup entropy).
- Pairing now prunes stale local peer records that share the newly-paired device's IP on the local network (old device-ids from before a reset, `manual-<ip>` placeholders), so the peer list doesn't accumulate cruft. Remote/NATed addresses are left alone since they can host several devices.
- Joining a cluster now tries at most 9 of its online devices; if none accept the PIN it asks you to try the PIN from a different device, keeping well under each device's pairing lockout threshold.
- Leaving/resetting a cluster now also resets the shared cluster-name history to this device, so the fresh cluster's name converges correctly once other devices join it (previously it kept the previous cluster's name-version origin, which pointed at a device no longer present).

## [0.3.7] - 2026-06-09

### Changed
- The Diagnostics Event Log no longer shows routine mTLS connect/drop activity at the Minimal level (these fire on every heartbeat, so they were continuous); they are now Detailed. Pairing events and handshake failures still surface at Minimal. Thanks to @mdunphy for the report.

### Fixed
- Large text could occasionally be reflected back to the sender when another copy followed it quickly. The echo guard tracked only the most recent self-write, so a follow-up copy unmasked the previous one's pending echo; it now tracks all recent self-writes. Large-payload dedup also keyed on a per-transfer id rather than content, so a reflected copy escaped it — dedup identity is now content-stable. Thanks to @mdunphy for the report.

## [0.3.6] - 2026-06-07

### Added
- Plain text over 10 MB now syncs out-of-band (streamed and zstd-compressed) instead of inline, so large text still pastes as text on the receiver; text over 100 MB is not shared and a notice is recorded in History.

### Changed
- History view no longer stalls on large clipboard items: the UI receives a light preview (truncated text or a thumbnail) while full content stays in a budgeted backend store (200 MB default, configurable under Settings → General). Copy/Send re-call large items by id. Thanks to @mdunphy for the report.

### Fixed
- The GNOME "clipboard sync is now active" notification no longer reappears on every launch. It now fires once as a recovery after the extension was missing/disabled, instead of on each cold-start backend promotion.
- A Windows peer could crash (heap corruption) when receiving a large text payload immediately followed by rich/HTML content. Rich-text and SVG clipboard writes now go through the same single worker thread as plain text and images, instead of opening the Windows clipboard from a second thread. Thanks to @mdunphy for the report.

## [0.3.5] — 2026-06-06

### Added
- Setting to disable configuring the Windows firewall at startup (default on, Windows only). Applies live when turned on; turning it off leaves any existing rule in place (#18).
- Setting to disable mDNS advertising (default on). Discovery of other devices keeps working; toggling applies live (#18).
- "Add Remote" with a single IP now connects directly when the peer is already paired, and only falls back to the PIN/pairing flow otherwise (#18).
- A Diagnostics event log (Settings → Diagnostics): an in-memory view of pairing and mTLS connect/drop events, filterable by level (Minimal/Detailed/Debug), with copy/clear/pause/auto-scroll. It is never written to disk. Thanks to @mdunphy for the suggestion.

### Fixed
- A general settings save no longer overwrites `pairing_accept_enabled`; the header-bar pairing toggle is now the sole writer, so a stale Settings tab can't clobber it.
- The cluster name is now shared across all devices instead of being per-device. Renaming on one device (or regenerating it) propagates to every peer, including peers that were offline at the time, so the name no longer silently diverges. Switching to an auto-generated name while in an active cluster now asks for confirmation first. Thanks to @mdunphy for the report.

### Changed
- The Settings page is now organised into a left sidebar with categories (General, Cluster, Files, Notifications, Diagnostics) instead of one long scroll. All existing settings are preserved; keyboard shortcuts now live under General (shown when the matching auto-send/receive is off).

### Security
- The device private key (`device_key.der`) and pairing PIN (`network_pin`) are now written with owner-only permissions (`0600`) on Linux and macOS, and existing files are re-hardened at startup. On Windows they rely on the default per-user `%APPDATA%` ACLs. Thanks to @mdunphy for the report.
- In Autogenerated mode the pairing PIN is no longer written to disk: it's generated in memory at each launch and any existing on-disk PIN is removed on startup, eliminating a stored-secret leak vector. Provisioned mode still persists a user-set PIN. Note: the auto-mode PIN now changes on every launch. Thanks to @mdunphy for the report.
- PIN values are no longer written to the on-disk log file. The verbose pairing-failure PIN dump now goes only to the in-memory Diagnostics event log (at Debug level), never to disk. The "Verbose pairing logs" setting still controls file-log failure detail (but never PINs). Thanks to @mdunphy for the report.

## [0.3.4] — 2026-05-30

### Fixed
- A "paired before this version's TLS upgrade — please re-pair" banner could appear for a manually-probed address that was never actually a paired device (e.g. a VPN gateway that forwards port 4654 to a real node). The startup sweep flagged any stored peer without a cert fingerprint as legacy, which also caught throwaway `manual-<ip>` probe placeholders. It now only flags genuine paired devices, so the banner can't fire for an address you can never re-pair with.
- Pairing now gives a clear "Failed to join network. The PIN may be incorrect." error when the PIN is wrong, instead of the misleading "Pairing session expired". Backend and Settings input also trim invisible trailing whitespace on the PIN, which used to silently break pairing. Enabling **Verbose pairing logs** dumps the responder's PIN bytes on failure for diagnosis.
- Rich-text sync to a Windows peer no longer clears the sender's clipboard (#17). The Windows clipboard write was wiping its own earlier formats on each step; pasting in Notepad, browsers, etc. on the Windows side now also works.
- Pasting a code copy from PyCharm into Word (HTML format) on a Windows peer now shows the whole snippet instead of the last few characters.
- A single space, newline, or tab on the clipboard is no longer broadcast or applied across the cluster — these had no useful content but would overwrite a peer's clipboard.
- Echo-suppression is more forgiving across any backend whose clipboard layer drops formats or the plain-text channel on the round-trip, closing a class of "truncated payload bounces back" bugs.

### Changed
- Receiving a rich-text payload on a **GNOME** machine now lands the plain text on the clipboard by default and fires a system notification with a one-click **"Switch to Rich"** action — no app window needed. This works around a hard GJS limitation that prevents the GNOME extension from offering multiple MIME types simultaneously (see [GJS #255](https://gitlab.gnome.org/GNOME/gjs/-/issues/255); the most-deployed [Clipboard Indicator](https://github.com/Tudmotu/gnome-shell-extension-clipboard-indicator) hits the same wall). Before this change, only the rich format reached the OS clipboard, so pastes into gedit, GNOME Text Editor, OnlyOffice, and browser inputs got nothing. Windows, macOS, and KDE/Sway/Hyprland receivers are unaffected — they write all MIME types atomically.
- Internal code cleanup, no behavior change: the two biggest source files were split into focused modules — the Rust backend (`lib.rs`, ~5,500 → ~870 lines) and the React UI (`App.tsx`, ~3,000 → ~1,230 lines). Deciding whether a peer's protocol version is compatible now happens in the Rust backend and is sent to the UI as a flag, instead of being re-implemented in TypeScript. Nothing user-facing changed; the work is purely about making the codebase easier to navigate and maintain.

## [0.3.3] — 2026-05-27

### Added
- Header-bar toggle to pause inbound pairing on demand. Green unlock = accepting, gray lock = paused. Setting persists across restarts. The same icon also turns rose when the existing brute-force lockout trips, so the header reflects the listener's actual state. Thanks to @mdunphy for the request (#16).

### Changed
- mDNS `proto` floor moves to `0.3.3` for the new pairing wire format. 0.3.1 peers surface in the existing "please upgrade" UI flow (per-peer amber-triangle indicator + modal on send). Frontend `MIN_COMPATIBLE_PROTOCOL` brought into sync with the backend floor (was stale at 0.3.0 since the 0.3.0 → 0.3.1 break). Wire and app/release versions are independent trackers — app 0.3.2 shipped with wire 0.3.1, so this is the next wire bump.

### Fixed
- Settings tab no longer spams `save_settings` once per second in the background. The autosave effect was re-firing on every post-save state sync because `initialSettings` got a fresh object reference each time, even when its value was unchanged. Now guarded by a value-based dirty check. Thanks to @mdunphy for the report (#15).

### Security
- Pairing-channel hardening (round-5 review with @mdunphy). New T2 `InitiatorKC` AEAD frame between SPAKE2-finish and the responder's encrypted identity reveal. The responder won't release any encrypted material until it AEAD-verifies the initiator's KC tag under the SPAKE2-derived `k_i2r`, so a wrong-PIN attacker can no longer harvest a `ResponderId` ciphertext, disconnect without sending a fingerprint, and brute-force PINs offline against it without depleting the failure budget. Wire-incompatible with 0.3.1.
- Pre-flight `proto` version check in the pair flow. mDNS-discovered peers below the compatibility floor are flagged in the existing "Peer needs updating" modal before the TCP pairing socket opens, instead of falling out as a generic timeout. Manual Add-Remote keeps the existing wire-level-failure path (no mDNS data, no advance signal).

## [0.3.2] — 2026-05-23

### Added
- "Clear" button on the Clipboard History view that wipes every entry from this device's history in one go, guarded by a confirmation dialog. The clear is local-only — other devices in the cluster keep their own history — so it's safe to use as a quick tidy-up without affecting peers. The existing per-item "Delete Everywhere" button still covers cluster-wide deletion when you need it. Thanks to @snaulh for the request.

### Fixed
- "Add Remote Peer" with a single IP now actually pairs. Previously the flow jumped straight to a QUIC/mTLS probe, which requires a cert fingerprint that only gets pinned *during* pairing — so any first-contact attempt against a peer where mDNS was blocked (firewall, VPN, restrictive LAN) failed with "no pinned fingerprint for <ip>; peer must re-pair", with no way out. Typing an IP now opens the existing PIN modal and runs the SPAKE2 pairing handshake over the plaintext-TCP pairing channel against the typed address — the same channel mDNS-discovered peers use. CIDR-range entries still trigger the existing subnet scan for rediscovering already-paired peers. As a side-effect cleanup, the locally-stored peer is now keyed on the SPAKE2-authenticated `device_id` from T2 rather than the caller-supplied id, matching what the responder already does on its T3 receive path. No wire-protocol change — an unmodified 0.3.x responder pairs correctly with a patched initiator. Thanks to @mdunphy for the report (#14).
- After joining a cluster you no longer appear as a peer of yourself. The responder generates its `ClusterInfo` reply *after* T3 has added you to its `known_peers`, so the snapshot it sends back included a record for you as seen from the responder's vantage point (whatever source IP the responder saw — e.g. a WireGuard tunnel address — with a placeholder hostname). The post-pairing import loop now drops any entry matching the local `device_id`, alongside the existing skip of the responder's own entry. Gossip and mDNS already filter self, so this only affected the immediate post-join window.

### Changed
- Renamed the "Cluster PIN" label to "My Cluster PIN" in the main cluster-info card and the Settings → Provisioned-mode editor, and added a one-line note under the provisioned-mode PIN field explaining that the PIN is local to the current device. Since v0.3.0, pairing imports cluster identity (`cluster_id`, `network_name`) but never the responder's PIN — each device keeps its own — and the old "Cluster PIN" wording implied a cluster-wide shared secret that doesn't exist. The Join modal's PIN field (which asks for the *target's* PIN, not yours) is unchanged for now.
- "My Cluster PIN" on the main screen is now hidden behind bullets by default; a new eye toggle next to the existing copy button reveals it on demand. The copy button still copies the real PIN regardless of whether it's currently revealed, so the muscle-memory "click copy, paste into the other device" flow is unchanged. Keeps the PIN off-screen during screen shares, screenshots, and over-the-shoulder glances by default — pairing flows where the PIN actually needs to be read out are still one click away.

## [0.3.1] — 2026-05-16

### Security
- Pairing-channel hardening (round-4 review with @mdunphy, see `WIRE-PROTOCOL-0.3.1.md`). Each pairing frame after SPAKE2 is now a single AEAD ciphertext under a role-distinct sub-key derived via HKDF from the SPAKE2 session key and the role-labelled transcript hash — no plaintext identity fields on the pairing channel, no separate confirm tag, AEAD decryption is the only thing that authenticates the inner payload. A wire-byte rewrite by an active MITM diverges each side's reconstructed transcript, diverges the sub-keys, and the next AEAD verify fails closed before any cert fingerprint is pinned.
- `Welcome` is gone from the pairing channel. Cluster bootstrap state (`cluster_id`, `known_peers`, `network_name`) now travels post-pairing over the already-authenticated QUIC/mTLS channel as `Message::ClusterInfoRequest` → `Message::ClusterInfo`. The pairing channel does one job now — pin cert fingerprints — and one job only.
- Global brute-force lockout on the pairing listener. After `PAIRING_FAILURE_LOCKOUT_THRESHOLD` (10) AEAD-decrypt failures aggregated across all source IPs, the responder shuts down inbound pairing entirely until the user re-arms via the new "Re-enable pairing" banner. The lockout also fires an urgent OS-level notification so the user notices immediately rather than only on next Settings open.
- `device_id` is now capped at 256 bytes with deterministic UTF-8-safe truncation on both ends, sizing the maximum pairing frame to a small fixed constant (`PAIRING_FRAME_CAP = 8 KB`, down from 256 KB).
- Single-flight pairing (cap = 1 concurrent inbound exchange) with a 10-second server-side idle timeout — an accepted-but-idle socket can no longer block other pairing attempts indefinitely.
- Pairing-log secrecy. The responder logs only a generic "Pairing failed from …" line on AEAD failure by default, so a passive log observer can't tell a wrong-PIN attempt apart from any other framing error. A new opt-in "Verbose pairing logs" toggle in Settings → Diagnostics surfaces the underlying diagnostic when needed.
- **Wire-format break.** The pairing channel is incompatible with 0.3.0 peers (no plaintext `device_id` at T0/T1, AEAD-wrapped `ResponderId`/`InitiatorId` at T2/T3, no `Welcome`). The mDNS `proto` floor moves to `0.3.1`; 0.3.0 peers get the same "please upgrade" UI we built for the 0.2.x → 0.3.0 break.

### Changed
- Rejected inbound QUIC handshakes now log the remote address alongside the failure reason. Previously the error line was anonymous, so a "fingerprint not in known peers" rejection gave no clue which host on the LAN was attempting to connect.

## [0.3.0] - 2026-05-10

### Added
- Image clipboard sync. Copy an image — for example with right-click → "Copy Image" in a browser, or from a screenshot tool — and it appears on your peers' clipboards, ready to paste into any app (Word, Preview, GIMP, etc.). Wired up across all four clipboard backends: X11, Wayland (KDE/Sway/Hyprland), GNOME Wayland (via the ClusterCut extension), Windows, and macOS. History view shows a thumbnail with dimensions and size. Up to 10 MB of encoded image bytes ride inline on `Message::Clipboard`; larger images switch automatically to the descriptor + file-transfer path described below.
- Large clipboard images (> 10 MB encoded) ride the existing file-transfer ALPN with a new "land on the OS clipboard, not on disk" hint. The sender writes the bytes to a temp file and broadcasts a *descriptor* on `Message::Clipboard` (`fetch_id` + `mime_type` + `total_size` + dims, no inline bytes). Receivers auto-fetch descriptors up to `max_auto_download_size` (default 50 MB) and surface a "Receiving Clipboard Image…" → "Image Available to Paste" notification pair; descriptors above that limit stash in pending-clipboard and surface an actionable "Large Clipboard Image — accept to receive" notification, gated by the same UI used for auto-receive=off. Newest-copy-wins race protection: if a fresh clipboard event arrives mid-fetch, the older fetch's bytes still drain off the wire (so QUIC stays happy) but are discarded instead of overwriting the OS clipboard.
- Rich-text clipboard sync. Copy formatted text — from Word, a browser, Pages, Apple Mail, etc. — and it appears on your peers' clipboards with formatting preserved. ClusterCut carries `text/html` and `text/rtf` alongside the existing plain-text path; receivers re-stock all three on their OS clipboard so the destination app can pick whichever format it understands best (Pages and Word pick RTF, Notepad falls back to plain text — the same buffet the source had). Wired across the wlroots Wayland (KDE/Sway/Hyprland), GNOME Wayland (via the ClusterCut extension), Windows, and macOS backends. **X11 rich-text is intentionally not supported** — X11's selection-ownership model would require a third long-running selection-owner thread alongside the existing text/files and image paths, which isn't worth the complexity for a declining display server. X11 still syncs text, files, and images as before, no regression. History view shows a "Rich · HTML, RTF" badge next to items that carry rich formats. 16 MB cap per format.
- GNOME extension v4.0: new D-Bus methods on the existing `Clipboard2` interface — `GetMimetypes`, `ReadBlob`, `WriteBlob`, `WriteFormats` — plus `BlobChanged` and `FormatsChanged` signals, used to relay image and rich-text clipboard data on GNOME Wayland. Fully backwards compatible — older apps that only know about text/files keep working unchanged with the new extension, and a 0.3.0 app paired with the older 3.0 extension silently falls back to text+files only.

### Fixed
- The Image Rebroadcast Issue: receiving a clipboard image (or rich-text payload) used to trigger one unintended rebroadcast back to the sender. The OS clipboard layer re-encodes bytes on round-trip (PNG via RGBA on macOS/Windows; line-ending and charset-declaration normalisation on rich text), and the byte-exact echo-suppression couldn't recognise the round-tripped payload as our own. Echo suppression now uses a stable fingerprint — `(mime, width, height)` for images, `(text, sorted MIME set)` for rich text — that survives the OS clipboard's re-encoding.
- IGNORED echo-suppression guard now auto-expires after 10 s if the expected echo never arrives. Previously a stuck guard (e.g. left over from an SVG paste) would generate spurious "variant differs" rebroadcasts on every subsequent unrelated clipboard event for the rest of the session.
- macOS Pages now pastes ClusterCut-relayed rich content as rich text. Previously Pages fell back to plain text because `public.utf8-plain-text` was declared first in `NSPasteboard.declareTypes:owner:`, which Pages treats as the canonical type. Rich-format types now come first in the declaration so Pages picks RTF/HTML.
- Windows clipboard contention during multi-MB image transfers. Clipboard reads and writes now serialise on a single worker thread holding the only `arboard::Clipboard` handle in the process, eliminating cross-thread `OpenClipboard` races that surfaced as `ERROR_CLIPBOARD_NOT_OPEN` (1418) during image set on Windows. Rich-text reads also gain an `IsClipboardFormatAvailable` precheck so the monitor doesn't open the Windows clipboard for every poll when no rich text is present.
- JPEG photos now ride the wire as `image/jpeg` verbatim instead of being decoded to RGBA and re-encoded as PNG. PNG is lossless and balloons photo content by 3–5× — a 30 MB JPEG was producing a ~143 MB PNG that overran the wire cap. Joins SVG and animated GIF in the passthrough MIME list.
- Windows JPEG and GIF dual-write — peers now receive a JPEG photo and paste it cleanly into Paint, Word, Photos, etc. Previously the receiver only registered the `image/jpeg` (or `image/gif`) format atom, which Chromium / Electron read but native Win32 apps ignore (those only look at `CF_DIB`/`CF_BITMAP`). The receive path now decodes the bytes to RGBA and writes `CF_DIB`/`CF_BITMAP`/the registered "PNG" atom via arboard, **and** appends the original `image/jpeg`/`image/gif` atom alongside without emptying the clipboard, so both native and modern apps paste correctly.
- Windows previously dropped many notifications silently — "Device joined" and the actionable "Files Available — accept to download" prompt would fail to appear when the system was busy (typical case: right after a peer setup or during a multi-MB file receive). Cause was a 5-second `SetExpirationTime` on the toast: if Windows hadn't rendered the banner by then, the toast was silently expired and dropped. Bumped to 10 minutes (still tidy in Action Center, comfortably past any queueing delay) and toast-creation / show / `CreateToastNotifierWithId` failures are now logged instead of swallowed.
- Windows passthrough-image read cap on the plugin backend was leaking the rich-text 16 MB cap onto image reads, so > 16 MB JPEGs / GIFs / SVGs got dropped at the source clipboard read with `Clipboard image/jpeg (… bytes) exceeds 16777216 byte cap; skipping.`. Bumped the image cap to 500 MB to match the absolute clipboard-image limit; rich-text stays at 16 MB.
- Windows firewall rule now covers all four directions ClusterCut needs: inbound + outbound UDP/4654 (QUIC steady-state traffic) and inbound + outbound TCP/4654 (the new plaintext-TCP pairing channel cohabiting with QUIC on the same numeric port). The 0.2.x rule was inbound-only on UDP, so on Defender / enterprise / "Block all outbound" private-profile configs Windows would accept incoming QUIC packets but fail to send the handshake reply — surfacing as "peer-A → Windows fails, Windows → peer-A works." Without the TCP additions, pairing initiated against a Windows machine would silently time out. The rule gains a `remoteip=any` scope and a versioned description sentinel; existing 0.2.x or earlier-0.3.0-pre rules trigger one UAC prompt on next launch to widen them.
- History view didn't scroll — its outer wrapper was a plain `space-y-5` div while the parent panel is `overflow-hidden`, so anything past the visible area was clipped (most noticeable once the list grew past a screenful of entries). Now mirrors SettingsView's `flex h-full flex-col gap-4 overflow-y-auto pb-4` wrapper.

### Security
- Added a strict Content Security Policy to the Tauri WebView (`default-src 'self'`, no `'unsafe-eval'`, no remote sources). Defense-in-depth against XSS in untrusted data the app renders (clipboard contents, filenames). Thanks to @mdunphy for the suggestion (#10).
- Pairing redesigned to run over a dedicated plaintext TCP channel rather than tunnelled inside an unauthenticated QUIC/TLS connection. SPAKE2 is a PAKE — it doesn't need transport-layer confidentiality, and wrapping it in unauth TLS only added complexity. Each device persists a stable self-signed cert/key. After SPAKE2 completes, both sides exchange a single key-confirmation tag (a fixed sentinel encrypted under the derived session key) before either side trusts the payload that follows — a wrong-PIN MITM derives a different key, the tag fails to decrypt, and pairing aborts before any cluster information is revealed. The `Welcome` and `PairFingerprint` payloads then travel as plaintext typed structs alongside the confirmation, exchanging cert fingerprints in both directions. The responder's network PIN is no longer in the payload at all (each device keeps its own PIN; pairing imports cluster identity but not the responder's PIN). Thanks to @mdunphy for the follow-up review (#9).
- Strict mutual TLS for all post-pairing traffic. Both peers present their pinned cert and reject the connection at the TLS handshake if the other side's cert fingerprint doesn't match an entry in `known_peers.json`. Critically, the TLS handshake-signature step is now validated against the cert via rustls's WebPKI — pre-fix the `verify_tls{12,13}_signature` methods returned `Ok(HandshakeSignatureValid::assertion())` unconditionally, so an attacker holding a copy of the legitimate (public) cert DER could stand up a TLS server presenting that cert with a different signing key and we'd accept it. Fingerprint match alone was insufficient. The opportunistic skip-verify fallback for legacy peers is removed; peers paired before this change cannot connect and are surfaced via a "please re-pair" banner on first launch. Thanks to @mdunphy for the report (#9).
- Application-layer ChaCha20-Poly1305 encryption is removed throughout. mTLS provides confidentiality and sender authenticity at the transport layer, making the previous double-encrypted shape redundant. Concretely: `cluster_key` and its 32-byte secret file (`cluster_key.bin`) are retired and wiped on first boot of v0.3+; per-clipboard and per-`FileRequest` payload encryption is gone (typed structs travel directly inside the QUIC stream); the per-file-stream `auth_token` is gone (the QUIC connection's mTLS already authenticates the sender); per-`PeerDiscovery` gossip signatures are gone (transitive trust through gossip from any mTLS-authenticated peer is sound under the same-operator threat model). A non-secret `cluster_id` (UUID) replaces `cluster_key`'s grouping role.
- Protocol-compatibility detection. Each device advertises its wire-protocol version in the mDNS `proto` TXT record. Discovered peers that don't advertise the property — or advertise a value below v0.3.0 — get a yellow warning triangle next to their hostname in both the cluster and discovery views (hover for an explanation), and the first time a clipboard send is attempted to one, a modal pops up naming the peer and asking the user to upgrade. Helps surface the upgrade requirement when a single device in a cluster lags behind. Known limitation: a peer that's mDNS-discoverable but unreachable for some other reason (firewall, transient outage) is detected only when the user actively triggers a send — generic per-peer connectivity tracking is out of scope.
- **Wire-format break.** Both the pairing channel (now plain TCP rather than QUIC) and the steady-state `Message` shape (typed payloads instead of `Vec<u8>` + cluster-key envelope) are incompatible with peers running 0.2.x or earlier-0.3.0-pre builds. No end users affected since 0.3.0 has not shipped. v0.3.0+ peers cannot pair with — or talk to — peers running older versions; the version check above surfaces the mismatch in the UI.

### Changed
- Bumped `@tauri-apps/api` to `~2.10.0` (#11).
- The "having trouble connecting?" modal at startup now suppresses itself when at least one of your manual peers is on a directly-reachable subnet. Previously any manual peer in `known_peers.json` would trigger the modal whenever no peers were online, even if you were sitting on the same LAN as that peer (in which case "no peers online" just means peers are offline, not a VPN/connectivity problem). Uses an approximate /24 same-subnet check against local interfaces.
- Receiving a clipboard payload from a peer now logs at INFO level (`Received clipboard text|image|rich from <sender>: …`), mirroring the existing `Sent clipboard to …` log on the outbound side. Auto-receive used to be silent at INFO, which made cross-machine debugging harder than it needed to be.

## [0.2.3] - 2026-05-08

### Added
- Optional zstd compression for file transfers, off by default. Enable it under Settings → File Transfer when transferring large, compressible files (text, code, logs, datasets) over slower links. Files smaller than 64 KB or already in a compressed format (images, video, archives, office docs, etc.) are skipped automatically. **Incompatible with ClusterCut 0.2.2 and earlier — peers running older versions will receive corrupt files when receiving from a sender that has compression enabled.** (#3)

### Changed
- Inter font is now vendored locally via `@fontsource/inter` instead of being fetched from Google Fonts at runtime, keeping the app fully LAN-only (#7).

## [0.2.2] - 2026-04-19

### Added
- GNOME Wayland: runtime detection of the ClusterCut extension. Clipboard sync now starts automatically as soon as the extension is enabled (no app restart required) and pauses cleanly if the extension is disabled or crashes.
- User-facing notifications when clipboard sync transitions between active and paused on GNOME Wayland.
- Cold-start notification on GNOME Wayland when the app launches without a working extension (missing, disabled, or outdated), so the user isn't left wondering why clipboard sync isn't working.
- GNOME Wayland: file-copy sync. Copying a file in Nautilus now syncs to other peers, and files received from peers paste cleanly into Nautilus as file copies (not text).
- GNOME: ClusterCut now reports its sync mode ("Auto", "Auto Send", "Auto Receive", "Auto Disabled") in the Background Apps list via the `org.freedesktop.portal.Background` portal, matching the extension's Quick Settings subtitle.
- `just extension-zip` now validates the generated zip with EGO's `shexli` tool before leaving it on disk; the zip is removed if validation fails.

### Fixed
- Wayland (all compositors): file pastes are now advertised with both `text/uri-list` and `x-special/gnome-copied-files`, so GTK file managers recognise them as file pastes instead of plain text.

### Changed
- GNOME extension D-Bus interface bumped to `app.clustercut.clustercut.Clipboard2` (extension version 3.0). An outdated extension no longer silently passes the clipboard-backend probe — the "Clipboard sync paused" notification fires until the matching extension version is installed.

## [0.2.1] - 2026-04-05

### Added
- Cross-platform network state monitor (`netmon`): detects suspend/resume and connectivity changes natively on Linux (logind + portal NetworkMonitor), Windows (WM_POWERBROADCAST + NetworkStatusChanged), and macOS (NSWorkspace + SCNetworkReachability).
- Universal heartbeat fallback: if 3 consecutive heartbeat rounds fail, the network is assumed down regardless of platform.
- Peer join verification: new peers are ping-verified before a "Device Joined" notification is shown; unverified joins are deferred until the peer responds.
- Peer leave retry: removal probes are retried (up to 3 times) when the local network is down, preventing false "Device Left" notifications.

### Fixed
- False "Device Left"/"Device Joined" notification bursts after resuming from suspend or reconnecting to Wi-Fi.
- Peers on a previous Wi-Fi network are now silently removed when the local IP changes (no notification noise on network switch).
- Windows: firewall rule is now checked before configuring, so the UAC prompt only appears on first launch (not every time).

### Changed
- Flatpak manifest now requests `org.freedesktop.login1` system bus access for suspend/resume detection.

## [0.2.0] - 2026-03-25

### Added
- Wayland clipboard support via a three-path architecture:
  - **X11**: existing tauri-plugin-clipboard (unchanged).
  - **Wayland + KDE/Sway/Hyprland**: native clipboard monitoring via wlr-data-control.
  - **Wayland + GNOME**: clipboard bridging through the ClusterCut GNOME extension over D-Bus.
- GNOME extension v2.0: now monitors and relays clipboard changes via St.Clipboard for Wayland sessions.
- Runtime detection of display server and automatic backend selection at startup.

### Changed
- Flatpak runtime bumped from GNOME 49 to GNOME 50.
- Flatpak is now Wayland-only (removed X11 and IPC socket permissions).
- Extension install prompt on GNOME Wayland is now a strong warning (extension is required for clipboard sync, not optional).

## [0.1.8] - 2026-03-25

### Fixed
- Flatpak: added X11 socket for clipboard support on Wayland (via XWayland).
- Tray icon now visible on KDE/Plasma dark panels (symbolic icon override is limited to GNOME-like desktops).

## [0.1.7] - 2026-03-18

### Fixed
- "Connecting to remote cluster" screen no longer appears on launch when using provisioned mode without manual peers.
- Leaving a provisioned cluster no longer shows the "connecting to remote cluster" spinner and timeout dialog.
- Settings screen now correctly reverts to "Autogenerated" mode after leaving a provisioned cluster.
- File transfers no longer stall over high-latency connections (e.g. VPN).

### Changed
- Flatpak: added read-only home directory access for file transfer from any location.

## [0.1.6] - 2026-02-24

### Fixed
- Tray icon now bundled in DEB/RPM packages (fixes wrong icon color on non-Flatpak installs).

### Changed
- Flatpak socket changed to `fallback-x11`.
- Added `just release` recipe for automated release builds.

## [0.1.5] - 2026-02-20

### Changed
- Flatpak notifications now use the Notification portal instead of direct D-Bus.
- License changed to GPL-3.0-or-later.

### Fixed
- Autostart command was double-wrapped by the Background portal.
- Window was invisible when starting minimized and then showing from tray.

## [0.1.1] - 2026-02-19

### Fixed
- Device join/leave notifications are more reliable.

## [0.1.0] - 2026-02-18

### Added
- Initial release.
