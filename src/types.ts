export interface Peer {
  id: string;
  ip: string;
  hostname: string;
  port: number;
  last_seen: number;
  is_trusted: boolean;
  is_manual?: boolean;
  network_name?: string;
  platform?: string; // Backend doesn't send this yet, will mock or infer
  /// Protocol-compatibility version advertised via mDNS.
  protocol_version?: string | null;
  /// True when the peer's `protocol_version` meets the minimum required by
  /// this build. Computed by the Rust backend (net_util::is_protocol_compatible)
  /// and injected into every frontend-bound payload; never travels peer-to-peer.
  compatible: boolean;
}

export type View = "devices" | "history" | "settings";

export type DiagLevel = "minimal" | "detailed" | "debug";

export interface DiagnosticEvent {
  ts_ms: number;
  level: DiagLevel;
  kind: string;
  peer: string | null;
  message: string;
}

export type NearbyNetwork = {
  networkName: string;
  devices: {
    id: string;
    hostname?: string;
    status: "online" | "offline";
    incompatible: boolean;
  }[];
};

export type ClipboardBlobPreview = {
  mime_type: string;
  width?: number;
  height?: number;
  size: number;        // byte length, for "12 KB" display
  // base64 PNG thumbnail from the backend (small). Absent for a not-yet-fetched
  // descriptor (no bytes available until the user accepts the transfer).
  thumbnail?: string;
  // §3.3 descriptor — bytes haven't been fetched yet; no thumbnail.
  descriptor?: boolean;
};

// Lightweight summary of an alternate clipboard format (text/html, text/rtf, …)
// used purely for the history badge. We deliberately don't carry the actual
// bytes through the React state — they ride on the underlying ClipboardPayload
// and get re-stocked on the OS clipboard by the backend; the UI just signals
// to the user that the item has rich formatting available.
export type ClipboardFormatPreview = {
  mime_type: string;
  binary: boolean;
  size: number; // length of the wire `data` string (base64 if binary, UTF-8 otherwise)
};

export type HistoryItem = {
  id: string;
  origin: "local" | "remote";
  device: string; // The sender's hostname
  ts: number; // Unix timestamp in seconds
  text: string;        // truncated preview (≤4 KB), NOT the full content
  text_len: number;    // true byte length of the full text
  files?: { name: string; size: number; }[];
  blob?: ClipboardBlobPreview;
  formats?: ClipboardFormatPreview[];
  sender_id?: string;
  has_backing: boolean; // re-call possible (content still retained)
};

export interface NotificationSettings {
  device_join: boolean;
  device_leave: boolean;
  data_sent: boolean;
  data_received: boolean;
}

export interface AppSettings {
  custom_device_name: string | null;
  cluster_mode: "auto" | "provisioned";
  auto_send: boolean;
  auto_receive: boolean;
  notifications: NotificationSettings;
  shortcut_send: string | null;
  shortcut_receive: string | null;
  enable_file_transfer: boolean;
  max_auto_download_size: number;
  notify_large_files: boolean;
  ignore_extension_missing: boolean;
  compress_file_transfers: boolean;
  pairing_debug_logs: boolean;
  configure_firewall: boolean;
  mdns_advertising: boolean;
  history_store_max_bytes: number; // bytes; History content store budget
}
