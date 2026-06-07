# Large plain-text clipboard sync (out-of-band) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Sync plain text larger than 10 MB by routing it through the existing image descriptor + file-transfer path (so it pastes as text), with a 100 MB ceiling and zstd compression on the wire.

**Architecture:** Reuse the §3.3 large-blob machinery (`stage_clipboard_blob_temp_file`, `ClipboardBlob::descriptor`, `DeliveryTarget::Clipboard`, the `clustercut-file` ALPN stream, the in-flight race guard, auto-receive/pending gating). The only new behavior is: the sender chooses inline / descriptor / too-large by text length; the receiver branches on MIME to land text vs. image; and the clipboard-blob stream path gains zstd encode/decode for `text/*` (mirroring the existing disk path).

**Tech Stack:** Rust, Tauri, quinn (QUIC), `async_compression` (zstd), serde_json.

Spec: `docs/superpowers/specs/2026-06-07-large-text-clipboard-design.md`

**Constants:** `MAX_CLIPBOARD_TEXT_WIRE_BYTES = 10 MB` (inline threshold), `MAX_CLIPBOARD_TEXT_BYTES = 100 MB` (absolute ceiling + receiver drain cap for `text/*`).

**Task order rationale:** the receiver must be able to handle (and decode) text before the sender starts emitting it. So: helper/consts → receiver landing (uncompressed) → receiver decode → sender emit → sender encode → tests/polish. Each commit leaves the tree working.

---

## File structure

- `src-tauri/src/clipboard/common.rs` — new consts; `text/plain` extension; a pure `text_wire_decision()` helper (testable); the `Text` branch of `process_clipboard_change` (inline / descriptor / too-large).
- `src-tauri/src/handlers.rs` — receiver landing branch + MIME-aware drain cap + zstd decode in `handle_incoming_clipboard_blob_stream`; zstd encode + `compressed` flag in the clipboard-blob FileRequest responder; generalize one log line.
- `src-tauri/src/protocol.rs` — unit tests only (descriptor + delivery-target round-trips for `text/plain`).

---

### Task 1: Constants, text extension, and the pure wire-decision helper

**Files:**
- Modify: `src-tauri/src/clipboard/common.rs` (consts near line 16-26; `extension_for_clipboard_mime` ~line 516; new helper + tests)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `common.rs` (find the existing one; if none in this file, add `#[cfg(test)] mod text_wire_tests { use super::*; ... }` at end of file):

```rust
#[cfg(test)]
mod text_wire_tests {
    use super::{text_wire_decision, TextWireDecision,
                MAX_CLIPBOARD_TEXT_WIRE_BYTES, MAX_CLIPBOARD_TEXT_BYTES};

    #[test]
    fn small_text_inlines() {
        assert_eq!(text_wire_decision(0), TextWireDecision::Inline);
        assert_eq!(text_wire_decision(1024), TextWireDecision::Inline);
        // Exactly at the inline threshold still inlines.
        assert_eq!(text_wire_decision(MAX_CLIPBOARD_TEXT_WIRE_BYTES), TextWireDecision::Inline);
    }

    #[test]
    fn medium_text_uses_descriptor() {
        assert_eq!(text_wire_decision(MAX_CLIPBOARD_TEXT_WIRE_BYTES + 1), TextWireDecision::Descriptor);
        assert_eq!(text_wire_decision(MAX_CLIPBOARD_TEXT_BYTES), TextWireDecision::Descriptor);
    }

    #[test]
    fn huge_text_is_too_large() {
        assert_eq!(text_wire_decision(MAX_CLIPBOARD_TEXT_BYTES + 1), TextWireDecision::TooLarge);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib text_wire_tests`
Expected: FAIL — `text_wire_decision` / `TextWireDecision` / consts not found.

- [ ] **Step 3: Add consts, extension case, and the helper**

In `common.rs`, after the existing `pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 500 * 1024 * 1024;` (~line 26), add:

```rust
/// Plain text at or below this size is inlined into `Message::Clipboard` as a
/// JSON string. Above it, the sender stages the text and broadcasts a
/// descriptor; peers fetch it over the `clustercut-file` ALPN (like big
/// images). Matches the image inline threshold.
pub const MAX_CLIPBOARD_TEXT_WIRE_BYTES: usize = 10 * 1024 * 1024;

/// Absolute ceiling for plain text. The sender will not share text larger than
/// this (it notifies instead), and the receiver caps its defensive stream
/// drain here for `text/*` blobs.
pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 100 * 1024 * 1024;

/// How a plain-text clipboard payload should travel, by decoded byte length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextWireDecision {
    /// Inline in the JSON message (small text, the common case).
    Inline,
    /// Stage + broadcast a descriptor; bytes ride the file-transfer ALPN.
    Descriptor,
    /// Too large to share at all — notify and skip.
    TooLarge,
}

/// Decide how a plain-text payload of `len` bytes should travel.
pub fn text_wire_decision(len: usize) -> TextWireDecision {
    if len <= MAX_CLIPBOARD_TEXT_WIRE_BYTES {
        TextWireDecision::Inline
    } else if len <= MAX_CLIPBOARD_TEXT_BYTES {
        TextWireDecision::Descriptor
    } else {
        TextWireDecision::TooLarge
    }
}
```

In `extension_for_clipboard_mime` (~line 516), add a `text/plain` arm before the `_ => "bin"`:

```rust
        "text/plain" => "txt",
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test --lib text_wire_tests`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/clipboard/common.rs
git commit -m "feat(clipboard): text wire-decision helper + size constants"
```

---

### Task 2: Receiver — land text vs image + MIME-aware drain cap

**Files:**
- Modify: `src-tauri/src/handlers.rs` — `handle_incoming_clipboard_blob_stream` (drain cap ~line 40; landing + history emit + notification ~lines 115-176)

No unit test (async network path); verified by the manual integration test in Task 7. This task makes the receiver able to land *uncompressed* `text/*` blobs.

- [ ] **Step 1: MIME-aware drain cap**

Replace the cap line (~line 40):

```rust
    let cap = crate::clipboard::common::MAX_CLIPBOARD_IMAGE_BYTES;
```

with:

```rust
    // text/* is capped tighter than images: the sender never sends >100 MB
    // text, so a peer streaming more than that is misbehaving — cut it off.
    let cap = if mime_type.starts_with("text/") {
        crate::clipboard::common::MAX_CLIPBOARD_TEXT_BYTES
    } else {
        crate::clipboard::common::MAX_CLIPBOARD_IMAGE_BYTES
    };
```

- [ ] **Step 2: Branch the landing on MIME (includes the history emit + notification)**

The original block has three parts that must stay together: land on clipboard (115-145), emit a `clipboard-change` history event (147-162), and notify (164-176). Because the text arm *moves* `accum` (into `String::from_utf8`) while the image arm *borrows* it (via `from_bytes`), and because the history entry must carry the text for text (spec decision (a)), fold all three parts into each branch.

Replace the entire block from the `// Reconstruct a ClipboardBlob …` comment (~line 115) through the closing `}` of the `if notifications.data_received { … }` (~line 176) with:

```rust
    let auto_recv = { state.settings.lock().unwrap().auto_receive };
    let notifications = state.settings.lock().unwrap().notifications.clone();
    let byte_len = accum.len();
    let mb = byte_len as f64 / (1024.0 * 1024.0);

    if mime_type.starts_with("text/") {
        // Decode strictly; mTLS + the size-match check above make corruption
        // near-impossible, so on a decode failure we drop rather than paste
        // mojibake.
        let text = match String::from_utf8(accum) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Large clipboard text did not decode as UTF-8: {}; dropping.", e);
                return;
            }
        };
        // History entry carries the full text (consistent with small text).
        let payload_event = crate::protocol::ClipboardPayload {
            id: header.id.clone(),
            text: text.clone(),
            files: None,
            blob: None,
            formats: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: format!("{}", addr),
            sender_id: String::new(),
        };
        if auto_recv {
            crate::clipboard::set_clipboard(&app, text);
        } else {
            let mut pending = state.pending_clipboard.lock().unwrap();
            *pending = Some(payload_event.clone());
        }
        let _ = app.emit("clipboard-change", &payload_event);
        if notifications.data_received {
            send_notification(
                &app,
                "Text Available to Paste",
                &format!("{:.1} MB of text is now on the clipboard.", mb),
                false, Some(3), "history", NotificationPayload::None,
            );
        }
    } else {
        // Reconstruct a ClipboardBlob and drive it onto the OS clipboard via the
        // same `set_clipboard_image` that the inline path uses.
        let blob = crate::protocol::ClipboardBlob::from_bytes(mime_type.clone(), &accum, width, height);
        let payload_event = crate::protocol::ClipboardPayload {
            id: header.id.clone(),
            text: String::new(),
            files: None,
            blob: Some(blob.clone()),
            formats: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sender: format!("{}", addr),
            sender_id: String::new(),
        };
        if auto_recv {
            crate::clipboard::set_clipboard_image(&app, blob);
        } else {
            let mut pending = state.pending_clipboard.lock().unwrap();
            *pending = Some(payload_event.clone());
        }
        let _ = app.emit("clipboard-change", &payload_event);
        if notifications.data_received {
            send_notification(
                &app,
                "Image Available to Paste",
                &format!("{:.1} MB image is now on the clipboard.", mb),
                false, Some(3), "history", NotificationPayload::None,
            );
        }
    }
```

NOTE: this preserves the original behavior for images exactly (emit `clipboard-change` always; set `pending_clipboard` only when auto-receive is off) and mirrors it for text. `byte_len`/`mb` are captured before `accum` is consumed. The image arm passes `blob` (not `blob.clone()`) to `set_clipboard_image` since `payload_event` already holds a clone.

- [ ] **Step 3: Build**

Run: `cd src-tauri && cargo build`
Expected: compiles (Linux build exercises this non-Windows-gated code).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/handlers.rs
git commit -m "feat(clipboard): receiver lands large text/* blobs as clipboard text"
```

---

### Task 3: Receiver — zstd decode on the clipboard-blob stream

**Files:**
- Modify: `src-tauri/src/handlers.rs` — `handle_incoming_clipboard_blob_stream` accumulate loop (~lines 41-78)

Mirrors the disk path's `ZstdDecoder` (handlers.rs:273-298). When `header.compressed`, decompress the stream before accumulating. `header.file_size` is the *uncompressed* length, so the existing size-match check (~line 87) stays correct.

- [ ] **Step 1: Wrap the reader when compressed**

Replace the accumulate loop (from `let mut buf = vec![0u8; 1024 * 1024];` through the closing `}` of the `loop { match reader.read(&mut buf).await { … } }`, ~lines 42-78) with a version that selects a decoder. Factor the per-chunk accumulation so both paths share it:

```rust
    let mut buf = vec![0u8; 1024 * 1024];
    let mut last_emit = std::time::Instant::now();
    let start_time = std::time::Instant::now();

    // Closure-free helper via a macro would over-engineer this; duplicate the
    // small loop like the disk path does, switching only the reader source.
    macro_rules! drain {
        ($src:expr) => {{
            let mut src = $src;
            loop {
                match src.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if accum.len() + n > cap {
                            tracing::error!(
                                "Clipboard-blob stream exceeds {} byte cap (got {}); dropping.",
                                cap, accum.len() + n
                            );
                            let mut sink = vec![0u8; 1024 * 1024];
                            while let Ok(n2) = src.read(&mut sink).await {
                                if n2 == 0 { break; }
                            }
                            return;
                        }
                        accum.extend_from_slice(&buf[..n]);
                        if last_emit.elapsed().as_millis() > 200 {
                            let _ = app.emit("file-progress", serde_json::json!({
                                "id": header.id,
                                "fileName": format!("Clipboard ({})", mime_type),
                                "total": header.file_size,
                                "transferred": accum.len() as u64,
                            }));
                            last_emit = std::time::Instant::now();
                        }
                    }
                    Err(e) => {
                        tracing::error!("Clipboard-blob stream read error: {}", e);
                        return;
                    }
                }
            }
        }};
    }

    if header.compressed {
        tracing::info!("[Receiver] Clipboard-blob ZSTD stream; expecting {} bytes (decompressed).", header.file_size);
        drain!(async_compression::tokio::bufread::ZstdDecoder::new(reader));
    } else {
        drain!(reader);
    }
```

NOTE: `reader` is `BufReader<quinn::RecvStream>`; `ZstdDecoder::new` takes a `BufRead` source, matching the disk path's usage at handlers.rs:275. `AsyncReadExt::read` is already in scope in this module (the disk path uses it). The macro is local to the function so it sees `accum`, `buf`, `cap`, `app`, `header`, `mime_type`.

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build`
Expected: compiles. If `async_compression` import differs, copy the exact `use`/path the disk decode path uses (handlers.rs:275).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/handlers.rs
git commit -m "feat(clipboard): zstd-decode compressed text blobs on receive"
```

---

### Task 4: Sender — emit descriptor / too-large for large text

**Files:**
- Modify: `src-tauri/src/clipboard/common.rs` — `process_clipboard_change`, `ClipboardContent::Text(text)` branch (~lines 597-640)

After this task, the sender produces (still-uncompressed) text descriptors; the receiver (Tasks 2-3) already handles them.

- [ ] **Step 1: Branch the Text arm on the wire decision**

In the `ClipboardContent::Text(text)` arm, the existing code skips whitespace-only text, updates `last_clipboard_content`, computes `hostname`/`msg_id`/`ts`/`local_id`, builds an inline `ClipboardPayload`, and calls `broadcast_clipboard`. Restructure so the inline build only happens for `Inline`:

Replace the block from `let hostname = crate::get_hostname_internal();` through `broadcast_clipboard(app_handle, state, transport, payload_obj);` (the tail of the Text arm) with:

```rust
            let hostname = crate::get_hostname_internal();
            let msg_id = uuid::Uuid::new_v4().to_string();
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let local_id = state.local_device_id.lock().unwrap().clone();

            match text_wire_decision(text.len()) {
                TextWireDecision::Inline => {
                    let payload_obj = ClipboardPayload {
                        id: msg_id,
                        text,
                        files: None,
                        blob: None,
                        formats: None,
                        timestamp: ts,
                        sender: hostname,
                        sender_id: local_id,
                    };
                    broadcast_clipboard(app_handle, state, transport, payload_obj);
                }
                TextWireDecision::Descriptor => {
                    let len = text.len() as u64;
                    match stage_clipboard_blob_temp_file(
                        app_handle, state, &msg_id, "text/plain", None, None, text.as_bytes(),
                    ) {
                        Ok(()) => {
                            tracing::info!(
                                "[ClipboardText] Large text ({} bytes) — broadcasting descriptor (id={})",
                                len, msg_id
                            );
                            let payload_obj = ClipboardPayload {
                                id: msg_id.clone(),
                                text: String::new(),
                                files: None,
                                blob: Some(ClipboardBlob::descriptor(
                                    "text/plain".to_string(),
                                    msg_id,
                                    len,
                                    None,
                                    None,
                                )),
                                formats: None,
                                timestamp: ts,
                                sender: hostname,
                                sender_id: local_id,
                            };
                            broadcast_clipboard(app_handle, state, transport, payload_obj);
                        }
                        Err(e) => {
                            tracing::error!("Failed to stage large clipboard text for descriptor path: {}", e);
                        }
                    }
                }
                TextWireDecision::TooLarge => {
                    tracing::warn!(
                        "Clipboard text is {} bytes (> {} cap); not sharing.",
                        text.len(), MAX_CLIPBOARD_TEXT_BYTES
                    );
                    crate::send_notification(
                        app_handle,
                        "Clipboard too large to share",
                        "The copied text is over 100 MB and was not sent to your cluster.",
                        false,
                        Some(4),
                        "history",
                        crate::NotificationPayload::None,
                    );
                }
            }
```

NOTE: confirm `ClipboardBlob::descriptor`'s exact signature (`descriptor(mime_type: String, fetch_id: impl Into<String>, total_size: u64, width: Option<u32>, height: Option<u32>)`) at protocol.rs:69 and match argument types/order. Confirm the `send_notification` signature (args: `app, title, body, is_action, dedupe_secs: Option<i32>, category, NotificationPayload`) against an existing call in `common.rs`/`handlers.rs` (e.g. handlers.rs:704) and adjust the literal args to match exactly.

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo build`
Expected: compiles. Fix any signature mismatches flagged by rustc against the real `ClipboardBlob::descriptor` / `send_notification`.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/clipboard/common.rs
git commit -m "feat(clipboard): sender streams >10MB text via descriptor, skips >100MB"
```

---

### Task 5: Sender — zstd-encode the text blob stream + set `compressed`

**Files:**
- Modify: `src-tauri/src/handlers.rs` — clipboard-blob FileRequest responder (~lines 1085-1140)

Mirrors the disk encode path (handlers.rs:1195-1222). Only `text/*` blobs are compressed; images keep `compressed: false`.

- [ ] **Step 1: Compute `compressed` and set it in the header**

In the responder, where `mime_type` is cloned from `meta` (~line 1059), add after it:

```rust
                                      let is_text = mime_type.starts_with("text/");
```

Change the header build (~line 1092) from the hardcoded `compressed: false` to:

```rust
                                                      compressed: is_text,
```

- [ ] **Step 2: Encode the stream when text**

Replace the raw send loop (from `let mut buf = vec![0u8; 1024 * 1024];` after the header write, through the `let _ = stream.finish(); drop(stream);` ~lines 1109-1131) with a branch mirroring the disk path:

```rust
                                                  let mut buf = vec![0u8; 1024 * 1024];
                                                  let start_time = std::time::Instant::now();
                                                  let mut chunks_sent = 0;
                                                  if is_text {
                                                      let mut encoder = async_compression::tokio::write::ZstdEncoder::with_quality(
                                                          stream,
                                                          async_compression::Level::Precise(crate::compression::ZSTD_LEVEL),
                                                      );
                                                      loop {
                                                          match file.read(&mut buf).await {
                                                              Ok(0) => break,
                                                              Ok(n) => {
                                                                  if let Err(e) = encoder.write_all(&buf[0..n]).await {
                                                                      tracing::error!("Clipboard-text compressed write error: {}", e);
                                                                      break;
                                                                  }
                                                                  chunks_sent += 1;
                                                              }
                                                              Err(e) => { tracing::error!("Clipboard-text file read error: {}", e); break; }
                                                          }
                                                      }
                                                      if let Err(e) = encoder.shutdown().await {
                                                          tracing::error!("Clipboard-text encoder shutdown error: {}", e);
                                                      }
                                                      let mut stream = encoder.into_inner();
                                                      let _ = stream.finish();
                                                      drop(stream);
                                                  } else {
                                                      loop {
                                                          match file.read(&mut buf).await {
                                                              Ok(0) => break,
                                                              Ok(n) => {
                                                                  if let Err(e) = stream.write_all(&buf[0..n]).await {
                                                                      tracing::error!("Clipboard-blob stream write error: {}", e);
                                                                      break;
                                                                  }
                                                                  chunks_sent += 1;
                                                              }
                                                              Err(e) => { tracing::error!("Clipboard-blob file read error: {}", e); break; }
                                                          }
                                                      }
                                                      let _ = stream.finish();
                                                      drop(stream);
                                                  }
                                                  let total_time = start_time.elapsed();
                                                  tracing::info!(
                                                      "[Sender] Clipboard-blob stream finished in {:?}. Chunks: {}", total_time, chunks_sent
                                                  );
```

Leave the post-loop `_connection.closed()` wait and trailing log unchanged.

- [ ] **Step 3: Build**

Run: `cd src-tauri && cargo build`
Expected: compiles. Match the `async_compression` import/paths to the disk encode path (handlers.rs:1197).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/handlers.rs
git commit -m "feat(clipboard): zstd-compress text blob stream on send"
```

---

### Task 6: Protocol round-trip tests for text descriptor + delivery target

**Files:**
- Modify: `src-tauri/src/protocol.rs` — extend the `#[cfg(test)] mod tests` (the existing descriptor test is at ~line 555; delivery-target test at ~line 671)

- [ ] **Step 1: Write the tests**

Add to the test module:

```rust
    #[test]
    fn clipboard_blob_text_descriptor_round_trips_through_json() {
        let blob = ClipboardBlob::descriptor("text/plain", "txt-1", 25_000_000, None, None);
        assert!(blob.is_descriptor());
        let json = serde_json::to_string(&blob).unwrap();
        let parsed: ClipboardBlob = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_descriptor());
        assert_eq!(parsed.mime_type, "text/plain");
        assert_eq!(parsed.total_size, Some(25_000_000));
        assert_eq!(parsed.width, None);
        assert_eq!(parsed.height, None);
    }

    #[test]
    fn file_stream_header_text_clipboard_target_round_trips() {
        let header = FileStreamHeader {
            id: "txt-1".to_string(),
            file_index: 0,
            file_name: "txt-1.txt".to_string(),
            file_size: 25_000_000,
            compressed: true,
            delivery_target: DeliveryTarget::Clipboard {
                mime_type: "text/plain".to_string(),
                width: None,
                height: None,
            },
        };
        let json = serde_json::to_string(&header).unwrap();
        let parsed: FileStreamHeader = serde_json::from_str(&json).unwrap();
        assert!(parsed.compressed);
        match parsed.delivery_target {
            DeliveryTarget::Clipboard { mime_type, width, height } => {
                assert_eq!(mime_type, "text/plain");
                assert_eq!(width, None);
                assert_eq!(height, None);
            }
            _ => panic!("expected Clipboard delivery target"),
        }
    }
```

NOTE: match field access (`total_size`, `mime_type`) to the actual `ClipboardBlob` definition (protocol.rs:25) and the existing image descriptor test (protocol.rs:555) — copy its exact accessor style.

- [ ] **Step 2: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib clipboard_blob_text_descriptor_round_trips_through_json file_stream_header_text_clipboard_target_round_trips`
Expected: PASS (2 tests). (If field names differ, fix to match the real struct, then re-run.)

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/protocol.rs
git commit -m "test(protocol): text/plain descriptor + clipboard delivery target round-trips"
```

---

### Task 7: Generalize the descriptor log + full test run + manual verification

**Files:**
- Modify: `src-tauri/src/handlers.rs` (~line 547-548 log string)

- [ ] **Step 1: Generalize the receive log**

Change the `"Received clipboard image descriptor from {}…"` log (~line 548) to `"Received clipboard descriptor from {}…"` (the MIME is already in the line, so it covers both image and text).

- [ ] **Step 2: Full library test run**

Run: `cd src-tauri && cargo test --lib`
Expected: all green, including the new `text_wire_tests` and protocol round-trips.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/handlers.rs
git commit -m "chore(clipboard): generalize descriptor log wording for text"
```

- [ ] **Step 4: Manual integration verification (two real peers; not automated)**

With a Fedora↔Windows (or any two) cluster, auto-receive ON, file transfer ON:

1. **Inline unchanged:** copy a short string → pastes on the peer instantly (regression check).
2. **Mid-size streams as text:** copy a ~20 MB text file's contents → after a brief transfer it pastes as **text** (CTRL+V) on the peer, not a file. Confirm the sender log shows "broadcasting descriptor" and the receiver log shows a ZSTD stream.
3. **Too-large skipped:** copy ~150 MB of text → sender does **not** broadcast; a "Clipboard too large to share" entry appears in **History**; the peer's clipboard is untouched.
4. **Settings gating:** with file transfer OFF on the receiver, a >10 MB text is not fetched (log: "File transfer disabled … Ignoring large clipboard descriptor"). With auto-download size below the text size, it waits for user confirm instead of auto-pasting.
5. **UTF-8 safety:** (optional) nothing to do — all real text is UTF-8; the decode-failure path only logs and drops.

---

## Self-review notes

- Spec coverage: inline/descriptor/too-large (Task 4) ✓; 10 MB threshold + 100 MB cap consts (Task 1) ✓; receiver text landing + CTRL+V (Task 2) ✓; MIME-aware drain cap (Task 2) ✓; zstd encode/decode (Tasks 3, 5) ✓; History notification (Task 4) ✓; full text in History — unchanged `payload_event` emission, inherited ✓; backward-compat — no wire change, inherited ✓; settings gating — inherited, documented (Task 7 manual) ✓; tests (Tasks 1, 6) ✓.
- Type consistency: `text_wire_decision`/`TextWireDecision`/consts defined Task 1, used Task 4; `mime_type.starts_with("text/")` used consistently in Tasks 2, 3, 5.
- The two "NOTE: confirm signature" callouts (`ClipboardBlob::descriptor`, `send_notification`) are deliberate verification points against real signatures the implementer must read — not placeholders for behavior.
