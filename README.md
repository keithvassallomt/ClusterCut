<p align="center">
  <img src="assets/README/logo.png" alt="ClusterCut Logo" width="120" />
</p>

# ClusterCut

> **Sync your clipboard, securely & locally.**

ClusterCut keeps your clipboard in sync across Windows, macOS, and Linux without your data ever leaving your local network. No clouds, no accounts, just seamless productivity.

VIDEO GOES HERE

## Features

### Seamless Clipboard Sync
Copy text on one device and paste it on another instantly, without any extra interactions. It just works.

<p align="center">
  <img src="assets/README/feature_seamless.png" alt="Seamless Clipboard Sync" width="80%" />
</p>

### Smart File Transfers
ClusterCut handles files intelligently. Small files are sent automatically—just copy and paste. For larger files, you'll receive a notification to download them when you're ready.

<p align="center">
  <img src="assets/README/feature_files.png" alt="Smart File Transfers" width="80%" />
</p>

### Clipboard History
Never lose a clip again. ClusterCut automatically saves your clipboard history, so you can access and paste previous items whenever you need them.

<p align="center">
  <img src="assets/README/feature_history.png" alt="Clipboard History" width="80%" />
</p>

### Manual Mode
Don't want to automatically send or receive everything? Enable manual mode to be in full control of your data flow.

<p align="center">
  <img src="assets/README/feature_manual.png" alt="Manual Mode" width="80%" />
</p>

### Works Remotely
Your cluster isn't limited to one network. Manually add remote clusters to sync over a VPN or the internet securely.

<p align="center">
  <img src="assets/README/feature_remote.png" alt="Works Remotely" width="80%" />
</p>

---

## Built for Privacy & Speed

| Feature | Description |
| :--- | :--- |
| **End-to-End Encryption** | Your clipboard content is encrypted before it leaves your device. Only your trusted devices can read it. |
| **Lightning Fast** | Built with Rust and optimized for local networks. Copy on one device, paste on another instantly. |
| **Cross-Platform** | Native experience on macOS, Windows, and Linux. Your clipboard works everywhere you do. |
| **Local Network Only** | No servers, no cloud, no internet required. Your data stays within your four walls. |
| **Zero Knowledge** | We don't collect data, telemetrics, or logs. Your privacy is our top priority. |
| **Open Source** | ClusterCut is fully open source. Inspect the code, contribute, or build it yourself. |

---

## Development

### Prerequisites

Before you begin, ensure you have the following installed:

- **Rust & Cargo**: [Install Rust](https://www.rust-lang.org/tools/install)
- **Node.js**: [Install Node.js](https://nodejs.org/) (Use LTS version)
- **Tauri Prerequisites**: Follow the [Tauri System Dependencies](https://v2.tauri.app/start/prerequisites/) guide for your OS.
  - **Linux**: requires `libwebkit2gtk-4.1-dev`, `build-essential`, `curl`, `wget`, `file`, `libssl-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`.

### Setup

1. **Clone the repository**:
   ```bash
   git clone https://github.com/keithvassallomt/ClusterCut.git
   cd ClusterCut
   ```

2. **Install dependencies**:
   ```bash
   npm install
   ```

### Running Locally

To start the development server with hot-reload:

```bash
npm run tauri dev
```

### Building

To build the application for production:

```bash
npm run tauri build
```

We also include a `Justfile` for common tasks (requires [just](https://github.com/casey/just)):

```bash
just build          # Build native package
just flatpak-local  # Build local Flatpak
just clean          # Clean build artifacts
```

### Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
