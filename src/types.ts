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
  /// Protocol-compatibility version advertised via mDNS. Missing or below
  /// `MIN_COMPATIBLE_PROTOCOL` means the peer is on a wire-protocol
  /// version this build can't talk to — surfaced as a yellow warning
  /// indicator and (on user-triggered sends) a "Peer needs updating" modal.
  protocol_version?: string | null;
}

export type View = "devices" | "history" | "settings";

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
  // Set on inline blobs that we decoded ourselves; absent for §3.3 descriptor
  // blobs whose bytes haven't been fetched yet (no thumbnail to show until the
  // user accepts and the file-transfer ALPN delivers the bytes).
  object_url?: string;
  // §3.3 descriptor — when true, `size` is the *total* expected size and
  // `object_url` is undefined. The user must accept (via the sync modal) to
  // trigger the file-transfer fetch; bytes then land on the OS clipboard
  // directly without a UI thumbnail.
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
  text: string;
  files?: { name: string; size: number; }[];
  blob?: ClipboardBlobPreview;
  formats?: ClipboardFormatPreview[];
  sender_id?: string;
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
}
