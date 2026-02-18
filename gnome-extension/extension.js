import Gio from 'gi://Gio';
import GObject from 'gi://GObject';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const DBUS_IFACE = `
<node>
  <interface name="com.keithvassallo.clustercut">
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
            'com.keithvassallo.clustercut',
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
            'com.keithvassallo.clustercut',
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
    }

    disable() {
        if (this._indicator) {
            this._indicator.quickSettingsItems.forEach(item => item.destroy());
            this._indicator.destroy();
            this._indicator = null;
        }
    }
}
