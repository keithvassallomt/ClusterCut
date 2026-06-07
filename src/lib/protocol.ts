import type { ClipboardBlobPreview, ClipboardFormatPreview } from "../types";

// Build the history blob preview from a backend ClipboardPreview.blob. The
// backend ships a small base64 PNG thumbnail (or none for a not-yet-fetched
// descriptor); we wrap it as a data URL. No full-bytes decode happens in the
// WebView anymore — that was the History perf bottleneck.
export function blobPreviewFromPreview(blob: any): ClipboardBlobPreview | undefined {
  if (!blob) return undefined;
  return {
    mime_type: blob.mime_type || "image/png",
    width: typeof blob.width === "number" ? blob.width : undefined,
    height: typeof blob.height === "number" ? blob.height : undefined,
    size: typeof blob.size === "number" ? blob.size : 0,
    thumbnail:
      typeof blob.thumbnail === "string" && blob.thumbnail.length > 0
        ? `data:image/png;base64,${blob.thumbnail}`
        : undefined,
    descriptor: !!blob.descriptor,
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
    if (!f || typeof f.mime_type !== "string") continue;
    // New light-preview shape ships `size` directly; old full-payload shape
    // carried `data` (base64/utf-8 string) and size was computed from its length.
    const size =
      typeof f.size === "number" ? f.size :
      typeof f.data === "string" ? f.data.length :
      0;
    list.push({
      mime_type: f.mime_type,
      binary: !!f.binary,
      size,
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
