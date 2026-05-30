import type { ClipboardBlobPreview, ClipboardFormatPreview } from "../types";

// Descriptor-vs-inline is a PROTOCOL decision owned by the backend: it sets
// `fetch_id` exactly when the bytes ride the file-transfer ALPN instead of
// inline (Rust `ClipboardBlob::is_descriptor()` == `fetch_id.is_some()`). The
// UI doesn't decide this — it only reads the backend's signal. Naming the check
// here keeps that rule in one place on the TS side rather than inlining the raw
// `fetch_id` test at every use.
function isDescriptorBlob(payloadBlob: any): boolean {
  return typeof payloadBlob.fetch_id === "string" && payloadBlob.fetch_id.length > 0;
}

// The backend ships ClipboardBlob.data as a base64 string (chosen over the
// default Vec<u8>→JSON-int-array encoding to keep wire size manageable —
// see protocol.rs). Decode once at receive time and stash an object URL so
// the thumbnail can render straight from memory. Caller is responsible for
// revoking the URL when the item is dropped.
export function blobFromPayload(payloadBlob: any): ClipboardBlobPreview | undefined {
  if (!payloadBlob) return undefined;

  // §3.3 descriptor — bytes ride the file-transfer ALPN, not inline. Surface
  // a preview without thumbnail so the pending UI / history list can render
  // "Large image (X.Y MB) — accept to receive". User accept triggers the fetch
  // through `confirm_pending_clipboard`, which the backend routes to a
  // `Message::FileRequest`.
  if (isDescriptorBlob(payloadBlob)) {
    return {
      mime_type: payloadBlob.mime_type || "image/png",
      width: typeof payloadBlob.width === "number" ? payloadBlob.width : undefined,
      height: typeof payloadBlob.height === "number" ? payloadBlob.height : undefined,
      size: typeof payloadBlob.total_size === "number" ? payloadBlob.total_size : 0,
      descriptor: true,
    };
  }

  if (typeof payloadBlob.data !== "string" || payloadBlob.data.length === 0) {
    return undefined;
  }
  let bytes: Uint8Array;
  try {
    const binString = atob(payloadBlob.data);
    bytes = Uint8Array.from(binString, c => c.charCodeAt(0));
  } catch (e) {
    console.warn("Failed to decode clipboard blob base64:", e);
    return undefined;
  }
  if (bytes.length === 0) return undefined;
  const url = URL.createObjectURL(new Blob([bytes], { type: payloadBlob.mime_type || "image/png" }));
  return {
    mime_type: payloadBlob.mime_type || "image/png",
    width: typeof payloadBlob.width === "number" ? payloadBlob.width : undefined,
    height: typeof payloadBlob.height === "number" ? payloadBlob.height : undefined,
    size: bytes.length,
    object_url: url,
  };
}

// Build the lightweight format summary used by the history "Rich text" badge.
// We don't decode or render the format bytes in the WebView — that would need
// HTML sanitization (no DOMPurify in deps) and runs counter to the strict CSP
// added in the security pass. The bytes stay on the underlying ClipboardPayload
// and get re-stocked onto the OS clipboard by the backend; the UI just
// surfaces "this item carries formatted content" to the user.
export function formatsFromPayload(payloadFormats: any): ClipboardFormatPreview[] | undefined {
  if (!Array.isArray(payloadFormats) || payloadFormats.length === 0) return undefined;
  const list: ClipboardFormatPreview[] = [];
  for (const f of payloadFormats) {
    if (!f || typeof f.mime_type !== "string" || typeof f.data !== "string") continue;
    list.push({
      mime_type: f.mime_type,
      binary: !!f.binary,
      size: f.data.length,
    });
  }
  return list.length > 0 ? list : undefined;
}

// Short label per MIME for the badge — falls back to the raw MIME for anything
// not in the curated list. New rich-text formats added in future would just
// show their MIME until added here.
export function shortRichLabel(mime: string): string {
  switch (mime) {
    case "text/html": return "HTML";
    case "text/rtf": return "RTF";
    case "image/svg+xml": return "SVG";
    default: return mime;
  }
}
