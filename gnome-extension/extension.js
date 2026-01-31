import Gio from 'gi://Gio';
import GObject from 'gi://GObject';
import St from 'gi://St';
import Gtk from 'gi://Gtk?version=4.0';
import Gdk from 'gi://Gdk?version=4.0';
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

const ClusterCutProxy = Gio.DBusProxy.makeProxyWrapper(DBUS_IFACE);

const ClusterCutIndicator = GObject.registerClass(
class ClusterCutIndicator extends QuickSettings.SystemIndicator {
    _init(extensionObject) {
        super._init();
        this._extensionObject = extensionObject;


        this._toggle = new QuickSettings.QuickMenuToggle({
            title: 'ClusterCut',
            toggleMode: true,
        });

        const iconPath = extensionObject.path + '/icons/hicolor/symbolic/apps/clustercut-symbolic.svg';
        console.log(`ClusterCut: Checking icon path: ${iconPath}`);
        const iconFile = Gio.File.new_for_path(iconPath);
        
        let iconSet = false;
        
        if (iconFile.query_exists(null)) {
             console.log('ClusterCut: Icon file exists. Setting GIcon.');
             const gicon = new Gio.FileIcon({ file: iconFile });
             this._toggle.gicon = gicon;
             iconSet = true;
        } else {
             console.log('ClusterCut: Icon file DOES NOT exist.');
        }
        
        if (!iconSet) {
             console.log('ClusterCut: Falling back to edit-paste-symbolic');
             this._toggle.iconName = 'edit-paste-symbolic';
        }
        
        this._toggle.subtitle = 'Searching...';
        this._toggle.checked = false; 

        // Add to the indicator's list
        this.quickSettingsItems.push(this._toggle);

        // ... (rest is same)
        this._proxy = new ClusterCutProxy(
            Gio.DBus.session,
            'com.keithvassallo.clustercut',
            '/org/gnome/Shell/Extensions/ClusterCut',
            (proxy, error) => {
                if (error) {
                    console.error('ClusterCut: Proxy creation failed', error);
                } else {
                    this._proxy.connectSignal('StateChanged', (proxy, senderName, [autoSend, autoReceive]) => {
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
                console.log(`ClusterCut: Connected to ${owner}`);
                this._appRunning = true;
                this._toggle.subtitle = 'Syncing...';
                this._toggle.reactive = true;
                this._updateState();
            },
            (conn, name) => {
                console.log('ClusterCut: App Lost/Not Found');
                this._appRunning = false;
                this._toggle.subtitle = 'Not running';
                this._toggle.checked = false;
                this._toggle.reactive = true; 
            }
        );

        // Connect Toggle Click
        this._toggle.connect('clicked', () => {
             if (!this._appRunning) {
                 this._toggle.subtitle = 'Launching...';
                 this._tryLaunchApp();
                 return;
             }

            if (this._proxy) {
                 const newState = this._toggle.checked;
                 
                 if (newState) {
                     // Enable Both
                     this._proxy.ToggleAutoSendRemote((res, err) => {
                          if (!err) {
                               this._proxy.ToggleAutoReceiveRemote((r, e) => {
                                   this._updateState();
                               });
                          } else {
                              console.error('ClusterCut: ToggleAutoSend failed', err);
                          }
                     });
                 } else {
                     // Disable Both
                     this._proxy.ToggleAutoSendRemote((res, err) => {
                          if (!err) {
                               this._proxy.ToggleAutoReceiveRemote((r, e) => {
                                   this._updateState();
                               });
                          } else {
                              console.error('ClusterCut: ToggleAutoSend failed', err);
                          }
                     });
                 }
            }
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

    _tryLaunchApp() {
        let appInfo = Gio.AppInfo.get_all().find(a => a.get_id() === 'com.keithvassallo.clustercut.desktop');
        if (appInfo) {
            appInfo.launch([], null);
        } else {
            try {
                Gio.AppInfo.create_from_commandline('clustercut', null, Gio.AppInfoCreateFlags.NONE).launch([], null);
            } catch (e) {
                console.error('Failed to launch ClusterCut', e);
                this._toggle.subtitle = 'Launch failed';
            }
        }
    }

    _updateInternalState(autoSend, autoReceive) {
        this._toggle.set({ checked: autoSend && autoReceive });
        
        // Update Menu Labels
        if (this._autoSendItem) {
            this._autoSendItem.label.text = autoSend ? 'Disable Auto-Send' : 'Enable Auto-Send';
        }
        if (this._autoReceiveItem) {
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
                console.error('ClusterCut: GetStateRemote failed', error);
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
                    console.error('ClusterCut: Unexpected result format' + JSON.stringify(result));
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
        this._toggle.destroy();
        this.emit('destroy');
    }
});

export default class ClusterCutExtension extends Extension {
    enable() {
        // Try to register Icon Path via Gtk.IconTheme
        // Explicitly check for display
        try {
            const display = Gdk.Display.get_default();
            if (display) {
                let theme = Gtk.IconTheme.get_for_display(display);
                if (!theme.get_search_path().includes(this.path + '/icons')) {
                    theme.add_search_path(this.path + '/icons');
                }
            } else {
                console.warn('ClusterCut: Gdk.Display.get_default() returned null. IconTheme registration skipped.');
            }
        } catch (e) {
             console.error('ClusterCut: IconTheme registration exception:', e);
        }

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
