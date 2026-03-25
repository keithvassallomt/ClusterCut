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

// D-Bus interface for clipboard bridging (Wayland)
const CLIPBOARD_DBUS_IFACE = `
<node>
  <interface name="app.clustercut.clustercut.Clipboard">
    <method name="ReadClipboard">
      <arg type="s" direction="out"/>
    </method>
    <method name="WriteClipboard">
      <arg type="s" direction="in"/>
    </method>
    <signal name="ClipboardChanged">
      <arg type="s"/>
    </signal>
  </interface>
</node>`;

const ClipboardBridgeIface = Gio.DBusNodeInfo.new_for_xml(CLIPBOARD_DBUS_IFACE);

const ClusterCutIndicator = GObject.registerClass(
class ClusterCutIndicator extends QuickSettings.SystemIndicator {
    _init(extensionObject) {
        super._init();
        this._extensionObject = extensionObject;

        // Initialize proxy class wrapper here instead of top-level
        this._ProxyClass = Gio.DBusProxy.makeProxyWrapper(DBUS_IFACE);

        this._toggle = new QuickSettings.QuickMenuToggle({
            title: 'ClusterCut',
            toggleMode: true,
        });

        // Set default/fallback initially to avoid blocking
        this._toggle.iconName = 'edit-paste-symbolic';
        this._toggle.subtitle = 'Searching...';
        this._toggle.checked = false;

        this._checkIcon(extensionObject.path);

        // Add to the indicator's list
        this.quickSettingsItems.push(this._toggle);

        this._proxy = new this._ProxyClass(
            Gio.DBus.session,
            'app.clustercut.clustercut',
            '/org/gnome/Shell/Extensions/ClusterCut',
            (proxy, error) => {
                if (error) {
                    // console.error('ClusterCut: Proxy creation failed', error);
                } else {
                    this._proxySignalId = this._proxy.connectSignal('StateChanged', (proxy, senderName, [autoSend, autoReceive]) => {
                         this._updateInternalState(autoSend, autoReceive);
                    });
                }
            }
        );

        // Watch for the App on D-Bus
        this._appRunning = false;
        this._watchId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            'app.clustercut.clustercut',
            Gio.BusNameWatcherFlags.NONE,
            (conn, name, owner) => {
                // console.log(`ClusterCut: Connected to ${owner}`);
                this._appRunning = true;
                this._toggle.subtitle = 'Syncing...';
                this._toggle.reactive = true;
                this._updateState();
            },
            (conn, name) => {
                // console.log('ClusterCut: App Lost/Not Found');
                this._appRunning = false;
                this._toggle.subtitle = 'Not running';
                this._toggle.checked = false;
                this._toggle.reactive = true; 
            }
        );

        // Connect Toggle Click
        this._toggleSignalId = this._toggle.connect('clicked', () => {
             if (!this._appRunning) {
                 this._toggle.subtitle = 'Not running';
                 return;
             }

             if (!this._proxy) return;

             // We just toggle, ignoring the specific checked state because the methods are Toggles
             this._proxy.ToggleAutoSendRemote((res, err) => {
                  if (!err) {
                       this._proxy.ToggleAutoReceiveRemote((r, e) => {
                           this._updateState();
                       });
                  } else {
                      // console.error('ClusterCut: ToggleAutoSend failed', err);
                  }
             });
        });

        // Add Menu Items
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
            // Async check using query_info_async
            await iconFile.query_info_async(Gio.FILE_ATTRIBUTE_STANDARD_NAME, Gio.FileQueryInfoFlags.NONE, GLib.PRIORITY_DEFAULT, null);
            
            // If we get here, file exists
            if (this._toggle) {
                 const gicon = new Gio.FileIcon({ file: iconFile });
                 this._toggle.gicon = gicon;
            }
        } catch (e) {
            // File likely doesn't exist or other error, fallback remains 'edit-paste-symbolic'
        }
    }

    _updateInternalState(autoSend, autoReceive) {
        if (!this._toggle) return;

        this._toggle.set({ checked: autoSend && autoReceive });
        
        // Update Menu Labels
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
             return; // Silent return to avoid log spam
        }

        this._proxy.GetStateRemote((result, error) => {
            if (error) {
                // console.error('ClusterCut: GetStateRemote failed', error);
                return;
            }
            if (result) {
                // With two 'out' args, result should be [val1, val2]
                let autoSend = false;
                let autoReceive = false;
                
                if (Array.isArray(result) && result.length >= 2) {
                     autoSend = result[0];
                     autoReceive = result[1];
                } else {
                    // console.error('ClusterCut: Unexpected result format' + JSON.stringify(result));
                }
                
                this._updateInternalState(autoSend, autoReceive);
            }
        });
    }
    
    destroy() {
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }

        // Clean up proxy signal
        if (this._proxySignalId && this._proxy) {
            this._proxy.disconnectSignal(this._proxySignalId);
            this._proxySignalId = null;
        }

        // Clean up toggle signal if we stored it (we didn't before, but now we should)
        if (this._toggleSignalId && this._toggle) {
            this._toggle.disconnect(this._toggleSignalId);
            this._toggleSignalId = null;
        }

        if (this._toggle) {
            this._toggle.destroy();
            this._toggle = null;
        }
        
        // Disconnect items
        if (this._autoSendItem) this._autoSendItem = null;
        if (this._autoReceiveItem) this._autoReceiveItem = null;

        super.destroy();
    }
});

export default class ClusterCutExtension extends Extension {
    enable() {
        this._indicator = new ClusterCutIndicator(this);
        Main.panel.statusArea.quickSettings.addExternalIndicator(this._indicator);

        // Start clipboard bridge D-Bus service
        this._startClipboardBridge();
    }

    disable() {
        this._stopClipboardBridge();

        if (this._indicator) {
            this._indicator.quickSettingsItems.forEach(item => item.destroy());
            this._indicator.destroy();
            this._indicator = null;
        }
    }

    _startClipboardBridge() {
        this._lastClipboardText = '';
        this._ignoreNextChange = false;

        // Export the clipboard D-Bus interface
        this._clipboardDbusId = Gio.DBus.session.register_object(
            '/org/gnome/Shell/Extensions/ClusterCut',
            ClipboardBridgeIface.interfaces[0],
            (connection, sender, objectPath, interfaceName, methodName, parameters, invocation) => {
                this._handleClipboardMethod(methodName, parameters, invocation);
            },
            null, // get_property
            null  // set_property
        );

        // Monitor clipboard changes via Meta.Selection
        const selection = global.display.get_selection();
        this._selectionOwnerChangedId = selection.connect(
            'owner-changed',
            (sel, selectionType, selectionSource) => {
                if (selectionType === Meta.SelectionType.SELECTION_CLIPBOARD) {
                    this._onClipboardOwnerChanged();
                }
            }
        );
    }

    _stopClipboardBridge() {
        if (this._selectionOwnerChangedId) {
            const selection = global.display.get_selection();
            selection.disconnect(this._selectionOwnerChangedId);
            this._selectionOwnerChangedId = null;
        }

        if (this._clipboardDbusId) {
            Gio.DBus.session.unregister_object(this._clipboardDbusId);
            this._clipboardDbusId = null;
        }

        this._lastClipboardText = '';
    }

    _onClipboardOwnerChanged() {
        if (this._ignoreNextChange) {
            this._ignoreNextChange = false;
            return;
        }

        const clipboard = St.Clipboard.get_default();
        clipboard.get_text(St.ClipboardType.CLIPBOARD, (cb, text) => {
            if (text && text !== this._lastClipboardText) {
                this._lastClipboardText = text;

                // Emit ClipboardChanged D-Bus signal
                Gio.DBus.session.emit_signal(
                    null, // broadcast
                    '/org/gnome/Shell/Extensions/ClusterCut',
                    'app.clustercut.clustercut.Clipboard',
                    'ClipboardChanged',
                    new GLib.Variant('(s)', [text])
                );
            }
        });
    }

    _handleClipboardMethod(methodName, parameters, invocation) {
        if (methodName === 'ReadClipboard') {
            const clipboard = St.Clipboard.get_default();
            clipboard.get_text(St.ClipboardType.CLIPBOARD, (cb, text) => {
                invocation.return_value(new GLib.Variant('(s)', [text || '']));
            });
        } else if (methodName === 'WriteClipboard') {
            const text = parameters.deep_unpack()[0];
            this._ignoreNextChange = true;
            this._lastClipboardText = text;
            const clipboard = St.Clipboard.get_default();
            clipboard.set_text(St.ClipboardType.CLIPBOARD, text);
            invocation.return_value(null);
        } else {
            invocation.return_dbus_error(
                'org.freedesktop.DBus.Error.UnknownMethod',
                `Unknown method: ${methodName}`
            );
        }
    }
}
