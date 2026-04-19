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
    <signal name="ClipboardChanged">
      <arg type="s"/>
    </signal>
    <signal name="FilesChanged">
      <arg type="as"/>
    </signal>
  </interface>
</node>`;

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
        } else {
            invocation.return_dbus_error(
                'org.freedesktop.DBus.Error.UnknownMethod',
                `Unknown method: ${methodName}`
            );
        }
    }
}
