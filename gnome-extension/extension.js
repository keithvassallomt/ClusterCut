// ClusterCut GNOME Shell extension.
// Bridges the Wayland clipboard to the ClusterCut app over D-Bus (St.Clipboard
// read/write is required for Wayland — see EGO-A-005 manual review note).
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Meta from 'gi://Meta';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const DBUS_IFACE = `
<node>
  <interface name="app.clustercut.clustercut">
    <method name="ToggleAutoSend">
      <arg type="b" direction="out"/>
    </method>
    <method name="ToggleAutoReceive">
      <arg type="b" direction="out"/>
    </method>
    <method name="GetState">
      <arg type="b" direction="out"/>
      <arg type="b" direction="out"/>
    </method>
    <signal name="StateChanged">
      <arg type="b"/>
      <arg type="b"/>
    </signal>
    <method name="ShowWindow"/>
    <method name="Quit"/>
  </interface>
</node>`;

// Versioned .Clipboard2 so the Rust is_available() probe can't be fooled by an
// older extension that only speaks the legacy text-only Clipboard interface.
//
// v4.0 additions to the Clipboard2 interface — added rather than bumped to a
// Clipboard3 because D-Bus is additive: old apps that only know
// ReadClipboard/WriteClipboard/ReadFiles/WriteFiles keep using them unchanged,
// while new apps probe for the v4.0 methods and silently fall back against
// an older extension. v4.0 carries:
//  - Image blob methods (ReadBlob / WriteBlob) and BlobChanged signal
//  - Rich-text format methods (WriteFormats) and FormatsChanged signal
// Both sets ride in the same v4.0 release so the EGO submission only happens
// once for the whole 0.3.0 cycle.
const CLIPBOARD_DBUS_IFACE = `
<node>
  <interface name="app.clustercut.clustercut.Clipboard2">
    <method name="ReadClipboard">
      <arg type="s" direction="out"/>
    </method>
    <method name="WriteClipboard">
      <arg type="s" direction="in"/>
    </method>
    <method name="ReadFiles">
      <arg type="as" direction="out"/>
    </method>
    <method name="WriteFiles">
      <arg type="as" direction="in"/>
    </method>
    <method name="GetMimetypes">
      <arg type="as" direction="out"/>
    </method>
    <method name="ReadBlob">
      <arg type="s" direction="in" name="mime_type"/>
      <arg type="ay" direction="out" name="data"/>
    </method>
    <method name="WriteBlob">
      <arg type="s" direction="in" name="mime_type"/>
      <arg type="ay" direction="in" name="data"/>
    </method>
    <method name="WriteFormats">
      <arg type="s" direction="in" name="text"/>
      <arg type="a(say)" direction="in" name="formats"/>
    </method>
    <signal name="ClipboardChanged">
      <arg type="s"/>
    </signal>
    <signal name="FilesChanged">
      <arg type="as"/>
    </signal>
    <signal name="BlobChanged">
      <arg type="s" name="mime_type"/>
      <arg type="ay" name="data"/>
    </signal>
    <signal name="FormatsChanged">
      <arg type="s" name="text"/>
      <arg type="a(say)" name="formats"/>
    </signal>
  </interface>
</node>`;

// Rich-text MIME types we relay alongside plain text. Same priority as the
// wlroots backend — text/html before text/rtf. Apps usually offer both when
// formatted text is on the clipboard.
//
// Strict allowlist: vendor-specific blobs like
// `application/x-qt-windows-mime;value="Native"`,
// `chromium/x-renderer-taint`, `org.chromium.web-custom-data`, and
// `text/_moz_htmlcontext` are never probed — they're either OS-internal,
// app-internal metadata, or duplicate of plain text.
const RICH_TEXT_MIME_PRIORITY = ['text/html', 'text/rtf'];

// 16 MB per-format cap, matches wlroots / Win / mac backends. Word HTML can be
// surprisingly large (lots of Office namespace metadata) but well under 1 MB
// in practice; this cap is well above expected values and well below the
// 64 MB transport per-message cap on the Rust side.
const MAX_RICH_TEXT_BYTES = 16 * 1024 * 1024;

// Prefix-aware MIME match: handles `text/html;charset=utf-8` (rare but
// spec-permitted) by treating it as `text/html`.
function findOfferedMime(offered, prefix) {
    if (offered.includes(prefix)) return prefix;
    const param = `${prefix};`;
    for (const m of offered) {
        if (m.startsWith(param)) return m;
    }
    return null;
}

// Image MIME types we relay over D-Bus, in preference order. Passthrough
// MIMEs (SVG vector, animated GIF) lead the list so a source that offers
// both passthrough + raster (e.g. Inkscape: image/svg+xml + raster PNG;
// some apps put image/gif + image/png) gives the receiving peer the
// higher-fidelity passthrough representation. The Rust side normalises
// raster sources to PNG before broadcasting; passthrough sources go
// over the wire verbatim.
const IMAGE_MIME_PRIORITY = [
    'image/svg+xml',
    'image/gif',
    'image/png',
    'image/jpeg',
    'image/webp',
    'image/bmp',
    'image/x-bmp',
    'image/tiff',
];

let ClipboardBridgeIface = null;

const ClusterCutIndicator = GObject.registerClass(
class ClusterCutIndicator extends QuickSettings.SystemIndicator {
    _init(extensionObject) {
        super._init();
        this._extensionObject = extensionObject;

        this._ProxyClass = Gio.DBusProxy.makeProxyWrapper(DBUS_IFACE);

        this._toggle = new QuickSettings.QuickMenuToggle({
            title: 'ClusterCut',
            toggleMode: true,
        });

        this._toggle.iconName = 'edit-paste-symbolic';
        this._toggle.subtitle = 'Searching...';
        this._toggle.checked = false;

        this._checkIcon(extensionObject.path);

        this.quickSettingsItems.push(this._toggle);

        this._proxy = new this._ProxyClass(
            Gio.DBus.session,
            'app.clustercut.clustercut',
            '/org/gnome/Shell/Extensions/ClusterCut',
            (proxy, error) => {
                if (!error) {
                    this._proxySignalId = this._proxy.connectSignal('StateChanged', (proxy, senderName, [autoSend, autoReceive]) => {
                         this._updateInternalState(autoSend, autoReceive);
                    });
                }
            }
        );

        this._appRunning = false;
        this._watchId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            'app.clustercut.clustercut',
            Gio.BusNameWatcherFlags.NONE,
            (conn, name, owner) => {
                this._appRunning = true;
                this._toggle.subtitle = 'Syncing...';
                this._toggle.reactive = true;
                this._updateState();
            },
            (conn, name) => {
                this._appRunning = false;
                this._toggle.subtitle = 'Not running';
                this._toggle.checked = false;
                this._toggle.reactive = true;
            }
        );

        this._toggle.connectObject('clicked', () => {
             if (!this._appRunning) {
                 this._toggle.subtitle = 'Not running';
                 return;
             }

             if (!this._proxy) return;

             this._proxy.ToggleAutoSendRemote((res, err) => {
                  if (!err) {
                       this._proxy.ToggleAutoReceiveRemote((r, e) => {
                           this._updateState();
                       });
                  }
             });
        }, this);

        this._toggle.menu.addAction('Show Window', () => {
            if (this._appRunning && this._proxy) this._proxy.ShowWindowRemote();
            Main.overview.hide();
            Main.panel.closeQuickSettings();
        });

        this._autoSendItem = this._toggle.menu.addAction('Enable Auto-Send', () => {
             if (this._appRunning && this._proxy) {
                 this._proxy.ToggleAutoSendRemote((result, error) => {
                      this._updateState();
                  });
             }
        });

        this._autoReceiveItem = this._toggle.menu.addAction('Enable Auto-Receive', () => {
             if (this._appRunning && this._proxy) {
                 this._proxy.ToggleAutoReceiveRemote((result, error) => {
                      this._updateState();
                  });
             }
        });

        this._toggle.menu.addAction('Quit', () => {
             if (this._appRunning && this._proxy) this._proxy.QuitRemote();
        });
    }

    async _checkIcon(extensionPath) {
        const iconPath = extensionPath + '/icons/hicolor/symbolic/apps/clustercut-symbolic.svg';
        const iconFile = Gio.File.new_for_path(iconPath);

        try {
            await iconFile.query_info_async(Gio.FILE_ATTRIBUTE_STANDARD_NAME, Gio.FileQueryInfoFlags.NONE, GLib.PRIORITY_DEFAULT, null);

            if (this._toggle) {
                 const gicon = new Gio.FileIcon({ file: iconFile });
                 this._toggle.gicon = gicon;
            }
        } catch (e) {
            // fallback remains 'edit-paste-symbolic'
        }
    }

    _updateInternalState(autoSend, autoReceive) {
        if (!this._toggle) return;

        this._toggle.set({ checked: autoSend && autoReceive });

        if (this._autoSendItem && this._autoSendItem.label) {
            this._autoSendItem.label.text = autoSend ? 'Disable Auto-Send' : 'Enable Auto-Send';
        }
        if (this._autoReceiveItem && this._autoReceiveItem.label) {
            this._autoReceiveItem.label.text = autoReceive ? 'Disable Auto-Receive' : 'Enable Auto-Receive';
        }

        let text = '';
        if (autoSend && autoReceive) {
            text = 'Auto';
        } else if (autoSend) {
            text = 'Auto Send';
        } else if (autoReceive) {
            text = 'Auto Receive';
        } else {
            text = 'Auto Disabled';
        }

        this._toggle.subtitle = text;
    }

    _updateState() {
        if (!this._proxy || !this._appRunning) {
             return;
        }

        this._proxy.GetStateRemote((result, error) => {
            if (error) {
                return;
            }
            if (result && Array.isArray(result) && result.length >= 2) {
                this._updateInternalState(result[0], result[1]);
            }
        });
    }

    destroy() {
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }

        // D-Bus proxy signals are not GObject signals — must use disconnectSignal.
        if (this._proxySignalId && this._proxy) {
            this._proxy.disconnectSignal(this._proxySignalId);
            this._proxySignalId = null;
        }

        if (this._toggle) {
            this._toggle.disconnectObject(this);
            this._toggle.destroy();
            this._toggle = null;
        }

        this._autoSendItem = null;
        this._autoReceiveItem = null;

        super.destroy();
    }
});

export default class ClusterCutExtension extends Extension {
    enable() {
        ClipboardBridgeIface = Gio.DBusNodeInfo.new_for_xml(CLIPBOARD_DBUS_IFACE);

        this._indicator = new ClusterCutIndicator(this);
        Main.panel.statusArea.quickSettings.addExternalIndicator(this._indicator);

        this._startClipboardBridge();
    }

    disable() {
        this._stopClipboardBridge();

        if (this._indicator) {
            this._indicator.quickSettingsItems.forEach(item => item.destroy());
            this._indicator.destroy();
            this._indicator = null;
        }

        ClipboardBridgeIface = null;
    }

    _startClipboardBridge() {
        this._lastClipboardText = '';
        this._lastFilesKey = '';
        this._lastBlobKey = '';
        this._lastFormatsKey = '';
        // GLib monotonic-time deadline. A one-shot bool doesn't work here because
        // a WriteFiles call writes two MIMEs and fires owner-changed more than once.
        this._ignoreUntil = 0;

        this._clipboardDbusId = Gio.DBus.session.register_object(
            '/org/gnome/Shell/Extensions/ClusterCut',
            ClipboardBridgeIface.interfaces[0],
            (connection, sender, objectPath, interfaceName, methodName, parameters, invocation) => {
                this._handleClipboardMethod(methodName, parameters, invocation);
            },
            null,
            null
        );

        const selection = global.display.get_selection();
        selection.connectObject(
            'owner-changed',
            (sel, selectionType, selectionSource) => {
                if (selectionType === Meta.SelectionType.SELECTION_CLIPBOARD) {
                    this._onClipboardOwnerChanged();
                }
            },
            this
        );
    }

    _stopClipboardBridge() {
        const selection = global.display.get_selection();
        selection.disconnectObject(this);

        if (this._clipboardDbusId) {
            Gio.DBus.session.unregister_object(this._clipboardDbusId);
            this._clipboardDbusId = null;
        }

        this._lastClipboardText = null;
        this._lastFilesKey = null;
        this._lastBlobKey = null;
        this._lastFormatsKey = null;
        this._ignoreUntil = 0;
    }

    _suppressNextChanges() {
        this._ignoreUntil = GLib.get_monotonic_time() + 500000;
    }

    _shouldIgnore() {
        return GLib.get_monotonic_time() < this._ignoreUntil;
    }

    _onClipboardOwnerChanged() {
        if (this._shouldIgnore()) {
            return;
        }

        // St.Clipboard.get_mimetypes is SYNCHRONOUS — passing a callback here
        // silently does nothing because the closure becomes the user_data arg.
        const clipboard = St.Clipboard.get_default();
        const mimetypes = clipboard.get_mimetypes(St.ClipboardType.CLIPBOARD);
        if (!mimetypes || mimetypes.length === 0) {
            return;
        }

        const hasUris = mimetypes.includes('text/uri-list')
            || mimetypes.includes('x-special/gnome-copied-files');

        if (hasUris) {
            this._readAndEmitFiles();
            return;
        }

        // Image probe sits between files and text so the canonical "Copy Image"
        // browser case is caught (no uri-list, no useful text), while the
        // existing "Copy a file" flow that does emit uri-list still wins above.
        const imageMime = IMAGE_MIME_PRIORITY.find(m => mimetypes.includes(m));
        if (imageMime) {
            this._readAndEmitBlob(imageMime);
            return;
        }

        // Rich-text probe — covers Word / browsers / Pages / Apple Mail etc.
        // sitting above plain text so we capture the formatted representation
        // alongside the plain-text fallback the source also offers. Each entry
        // in richMimes is { canonical, offered } so we can pass the actual
        // advertised MIME to St.Clipboard.get_content but emit the canonical
        // prefix in the FormatsChanged signal for receivers.
        const richMimes = [];
        for (const prefix of RICH_TEXT_MIME_PRIORITY) {
            const offered = findOfferedMime(mimetypes, prefix);
            if (offered) richMimes.push({ canonical: prefix, offered });
        }
        if (richMimes.length > 0) {
            this._readAndEmitFormats(richMimes);
            return;
        }

        const hasText = mimetypes.some(m => m === 'text/plain'
            || m === 'text/plain;charset=utf-8'
            || m === 'UTF8_STRING'
            || m === 'STRING');

        if (hasText) {
            this._readAndEmitText();
        }
    }

    _readAndEmitText() {
        const clipboard = St.Clipboard.get_default();
        clipboard.get_text(St.ClipboardType.CLIPBOARD, (cb, text) => {
            if (text && text !== this._lastClipboardText) {
                this._lastClipboardText = text;
                this._lastFilesKey = '';

                Gio.DBus.session.emit_signal(
                    null,
                    '/org/gnome/Shell/Extensions/ClusterCut',
                    'app.clustercut.clustercut.Clipboard2',
                    'ClipboardChanged',
                    new GLib.Variant('(s)', [text])
                );
            }
        });
    }

    _readAndEmitFiles() {
        const clipboard = St.Clipboard.get_default();
        clipboard.get_content(St.ClipboardType.CLIPBOARD, 'text/uri-list', (cb, bytes) => {
            const uris = this._parseUriList(bytes);
            if (uris.length === 0) {
                return;
            }

            const key = uris.join('\n');
            if (key === this._lastFilesKey) {
                return;
            }
            this._lastFilesKey = key;
            this._lastClipboardText = '';

            Gio.DBus.session.emit_signal(
                null,
                '/org/gnome/Shell/Extensions/ClusterCut',
                'app.clustercut.clustercut.Clipboard2',
                'FilesChanged',
                new GLib.Variant('(as)', [uris])
            );
        });
    }

    // Stable, cheap fingerprint for a blob so we can dedup natural re-copies of
    // the same content without holding the full bytes for comparison. Mirrors
    // the Rust-side signature in clipboard/common.rs.
    _blobKey(mime, data) {
        if (!data || data.length === 0) {
            return '';
        }
        const head = Math.min(16, data.length);
        const tailStart = Math.max(0, data.length - 16);
        let sig = `${mime}:${data.length}:`;
        for (let i = 0; i < head; i++) {
            sig += data[i].toString(16);
        }
        sig += ':';
        for (let i = tailStart; i < data.length; i++) {
            sig += data[i].toString(16);
        }
        return sig;
    }

    _readAndEmitBlob(mime) {
        const clipboard = St.Clipboard.get_default();
        clipboard.get_content(St.ClipboardType.CLIPBOARD, mime, (cb, bytes) => {
            if (!bytes) {
                return;
            }
            const data = bytes.get_data ? bytes.get_data() : bytes;
            if (!data || data.length === 0) {
                return;
            }

            const key = this._blobKey(mime, data);
            if (key === this._lastBlobKey) {
                return;
            }
            this._lastBlobKey = key;
            this._lastClipboardText = '';
            this._lastFilesKey = '';

            Gio.DBus.session.emit_signal(
                null,
                '/org/gnome/Shell/Extensions/ClusterCut',
                'app.clustercut.clustercut.Clipboard2',
                'BlobChanged',
                new GLib.Variant('(say)', [mime, data])
            );
        });
    }

    // Stable fingerprint for a rich-text snapshot so we don't re-emit when the
    // same selection is re-copied. Sums each format's size and a short head/
    // tail hex of its bytes — same shape as _blobKey.
    _formatsKey(text, formats) {
        let sig = `text:${text ? text.length : 0}|`;
        for (const [mime, data] of formats) {
            sig += `${mime}:${data.length}:`;
            const head = Math.min(8, data.length);
            const tailStart = Math.max(0, data.length - 8);
            for (let i = 0; i < head; i++) sig += data[i].toString(16);
            sig += '/';
            for (let i = tailStart; i < data.length; i++) sig += data[i].toString(16);
            sig += ';';
        }
        return sig;
    }

    // Read every available rich-text format alongside the plain text and emit
    // them as a single FormatsChanged signal so the Rust side sees a coherent
    // snapshot of one copy event. St.Clipboard.get_content is async per MIME,
    // so we chain callbacks; missing formats are silently skipped.
    _readAndEmitFormats(richMimes) {
        const clipboard = St.Clipboard.get_default();
        const collected = [];
        let pending = richMimes.length;

        const finish = () => {
            // Plain text last so we have it in the same emission. If the
            // source didn't offer text/plain we send an empty string.
            clipboard.get_text(St.ClipboardType.CLIPBOARD, (cb, text) => {
                const t = text || '';
                if (collected.length === 0) {
                    return;
                }
                const key = this._formatsKey(t, collected);
                if (key === this._lastFormatsKey) {
                    return;
                }
                this._lastFormatsKey = key;
                this._lastClipboardText = '';
                this._lastFilesKey = '';
                this._lastBlobKey = '';

                Gio.DBus.session.emit_signal(
                    null,
                    '/org/gnome/Shell/Extensions/ClusterCut',
                    'app.clustercut.clustercut.Clipboard2',
                    'FormatsChanged',
                    new GLib.Variant('(sa(say))', [t, collected])
                );
            });
        };

        for (const entry of richMimes) {
            clipboard.get_content(St.ClipboardType.CLIPBOARD, entry.offered, (cb, bytes) => {
                if (bytes) {
                    const data = bytes.get_data ? bytes.get_data() : bytes;
                    if (data && data.length > 0) {
                        if (data.length > MAX_RICH_TEXT_BYTES) {
                            console.warn(
                                `Clipboard ${entry.offered} exceeds ${MAX_RICH_TEXT_BYTES} byte cap; skipping format.`
                            );
                        } else {
                            // Emit canonical MIME so receivers see `text/html`
                            // rather than `text/html;charset=utf-8` etc.
                            collected.push([entry.canonical, data]);
                        }
                    }
                }
                pending -= 1;
                if (pending === 0) {
                    finish();
                }
            });
        }
    }

    _parseUriList(bytes) {
        if (!bytes) {
            return [];
        }
        const data = bytes.get_data ? bytes.get_data() : bytes;
        if (!data || data.length === 0) {
            return [];
        }
        let text;
        try {
            text = new TextDecoder('utf-8').decode(data);
        } catch (_e) {
            return [];
        }
        return text
            .split(/\r?\n/)
            .map(l => l.trim())
            .filter(l => l.length > 0 && !l.startsWith('#'));
    }

    _writeFiles(uris) {
        if (!uris || uris.length === 0) {
            return;
        }

        this._suppressNextChanges();
        this._lastFilesKey = uris.join('\n');
        this._lastClipboardText = '';

        const clipboard = St.Clipboard.get_default();

        // Write both MIMEs: Nautilus/GTK file managers key on
        // x-special/gnome-copied-files to decide "paste file" vs "paste text".
        const uriListText = uris.join('\n') + '\n';
        const gnomeCopiedText = 'copy\n' + uris.join('\n');

        const encoder = new TextEncoder();
        const uriListBytes = GLib.Bytes.new(encoder.encode(uriListText));
        const gnomeCopiedBytes = GLib.Bytes.new(encoder.encode(gnomeCopiedText));

        clipboard.set_content(St.ClipboardType.CLIPBOARD, 'text/uri-list', uriListBytes);
        clipboard.set_content(St.ClipboardType.CLIPBOARD, 'x-special/gnome-copied-files', gnomeCopiedBytes);
    }

    _writeBlob(mime, data) {
        if (!data || data.length === 0) {
            return;
        }

        this._suppressNextChanges();
        this._lastBlobKey = this._blobKey(mime, data);
        this._lastClipboardText = '';
        this._lastFilesKey = '';

        const clipboard = St.Clipboard.get_default();
        const glibBytes = GLib.Bytes.new(data);
        clipboard.set_content(St.ClipboardType.CLIPBOARD, mime, glibBytes);
    }

    // Atomically restock the clipboard with plain text plus alternate
    // representations (text/html, text/rtf, …). Single _suppressNextChanges
    // covers the whole batch — owner-changed fires once per set call but we
    // only want to skip our own writes, not real user activity. St.Clipboard
    // accumulates MIMEs across set_content/set_text calls (see _writeFiles
    // for the same pattern with text/uri-list + x-special/gnome-copied-files).
    _writeFormats(text, formats) {
        if (!Array.isArray(formats)) {
            return;
        }

        this._suppressNextChanges();
        this._lastFormatsKey = this._formatsKey(text || '', formats);
        this._lastClipboardText = '';
        this._lastFilesKey = '';
        this._lastBlobKey = '';

        const clipboard = St.Clipboard.get_default();
        if (text) {
            clipboard.set_text(St.ClipboardType.CLIPBOARD, text);
        }
        for (const [mime, data] of formats) {
            if (!data || data.length === 0) {
                continue;
            }
            const glibBytes = GLib.Bytes.new(data);
            clipboard.set_content(St.ClipboardType.CLIPBOARD, mime, glibBytes);
        }
    }

    _handleClipboardMethod(methodName, parameters, invocation) {
        if (methodName === 'ReadClipboard') {
            const clipboard = St.Clipboard.get_default();
            clipboard.get_text(St.ClipboardType.CLIPBOARD, (cb, text) => {
                invocation.return_value(new GLib.Variant('(s)', [text || '']));
            });
        } else if (methodName === 'WriteClipboard') {
            const text = parameters.deep_unpack()[0];
            this._suppressNextChanges();
            this._lastClipboardText = text;
            this._lastFilesKey = '';
            const clipboard = St.Clipboard.get_default();
            clipboard.set_text(St.ClipboardType.CLIPBOARD, text);
            invocation.return_value(null);
        } else if (methodName === 'ReadFiles') {
            const clipboard = St.Clipboard.get_default();
            clipboard.get_content(St.ClipboardType.CLIPBOARD, 'text/uri-list', (cb, bytes) => {
                const uris = this._parseUriList(bytes);
                invocation.return_value(new GLib.Variant('(as)', [uris]));
            });
        } else if (methodName === 'WriteFiles') {
            const [uris] = parameters.deep_unpack();
            this._writeFiles(uris);
            invocation.return_value(null);
        } else if (methodName === 'GetMimetypes') {
            // Synchronous — see _onClipboardOwnerChanged comment.
            const clipboard = St.Clipboard.get_default();
            const mimetypes = clipboard.get_mimetypes(St.ClipboardType.CLIPBOARD) || [];
            invocation.return_value(new GLib.Variant('(as)', [mimetypes]));
        } else if (methodName === 'ReadBlob') {
            const [mime] = parameters.deep_unpack();
            const clipboard = St.Clipboard.get_default();
            clipboard.get_content(St.ClipboardType.CLIPBOARD, mime, (cb, bytes) => {
                let data = new Uint8Array(0);
                if (bytes) {
                    const arr = bytes.get_data ? bytes.get_data() : bytes;
                    if (arr && arr.length > 0) {
                        data = arr;
                    }
                }
                invocation.return_value(new GLib.Variant('(ay)', [data]));
            });
        } else if (methodName === 'WriteBlob') {
            const [mime, data] = parameters.deep_unpack();
            this._writeBlob(mime, data);
            invocation.return_value(null);
        } else if (methodName === 'WriteFormats') {
            const [text, formats] = parameters.deep_unpack();
            this._writeFormats(text, formats);
            invocation.return_value(null);
        } else {
            invocation.return_dbus_error(
                'org.freedesktop.DBus.Error.UnknownMethod',
                `Unknown method: ${methodName}`
            );
        }
    }
}
