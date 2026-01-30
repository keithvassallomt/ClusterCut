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
      <arg type="(bb)" direction="out"/>
    </method>
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

        // Create the Toggle Button
        this._toggle = new QuickSettings.QuickMenuToggle({
            title: 'ClusterCut',
            toggleMode: true,
            iconName: 'edit-paste-symbolic',
        });
        
        this._toggle.subtitle = 'Connecting...';

        // Add to the indicator's list (this makes it show up in Quick Settings)
        this.quickSettingsItems.push(this._toggle);

        // D-Bus Proxy Init
        this._proxy = new ClusterCutProxy(
            Gio.DBus.session,
            'com.keithvassallo.clustercut',
            '/org/gnome/Shell/Extensions/ClusterCut',
            (proxy, error) => {
                if (error) {
                    console.error('ClusterCut: Failed to connect to D-Bus', error);
                    this._toggle.subtitle = 'Error';
                    return;
                }
                this._updateState();
            }
        );

        // Connect Toggle Click
        this._toggle.connect('clicked', () => {
            if (this._proxy) {
                 this._proxy.ToggleAutoSendRemote((result, error) => {
                      if (error) {
                          console.error('ClusterCut: Error calling ToggleAutoSend', error);
                      } else {
                          this._updateState();
                      }
                 });
            }
        });

        // Add Menu Items (Arrow Expand)
        this._toggle.menu.addAction('Show Window', () => {
            if (this._proxy) this._proxy.ShowWindowRemote();
            Main.overview.hide();
            Main.panel.closeQuickSettings();
        });

        this._toggle.menu.addAction('Toggle Auto-Receive', () => {
             if (this._proxy) {
                 this._proxy.ToggleAutoReceiveRemote((result, error) => {
                      this._updateState();
                 });
             }
        });

        this._toggle.menu.addAction('Quit', () => {
             if (this._proxy) this._proxy.QuitRemote();
        });
        
        // Polling loop for state (fallback if signals aren't used)
        // Ideally we should listen for PropertiesChanged or a Signal from the service,
        // but for now we'll update on open or click.
    }

    _updateState() {
        if (!this._proxy) return;
        this._proxy.GetStateRemote((result, error) => {
            if (result && !error) {
                const [autoSend, autoReceive] = result[0]; // Wrapper returns array
                
                this._toggle.set({ checked: autoSend });
                
                let sub = [];
                if (autoSend) sub.push('Sending');
                else sub.push('Paused');
                
                if (autoReceive) sub.push('Receiving');
                
                this._toggle.subtitle = sub.join(' & ');
            }
        });
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
