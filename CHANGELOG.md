# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
