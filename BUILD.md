# Build Instructions

We use `just` to standardize build commands across platforms.

## Prerequisites
1.  **Node.js** (v18+)
2.  **Rust** (Stable)
3.  **Just**: `cargo install just` (or via your package manager)
4.  **Tauri system dependencies**: Follow the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS.
    - **Linux (Fedora/RHEL):** `webkit2gtk4.1-devel openssl-devel gtk3-devel libappindicator-gtk3-devel librsvg2-devel`
    - **Linux (Debian/Ubuntu):** `libwebkit2gtk-4.1-dev build-essential libssl-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`

## Justfile Recipes

| Recipe | Description |
| :--- | :--- |
| `just build` | Build the native installer for your OS (`.exe`, `.dmg`, `.deb/.rpm`) |
| `just flatpak` | Build and install a local Flatpak from the working tree |
| `just run-flatpak` | Run the locally-installed Flatpak |
| `just extension-zip` | Build the GNOME extension ZIP for EGO submission |
| `just bump-version` | Interactively bump the version across all project files |
| `just release` | Full release workflow: version sync, commit, tag, push, build native + Flatpak |
| `just friendlyhub-update` | Prepare a FriendlyHub submission (manifest, sources, metainfo) |
| `just clean` | Remove all build artifacts |

## Native Build (Windows/macOS/Linux)

```bash
just build
```

**Output:** `src-tauri/target/release/bundle/`

## Flatpak (Linux Only)

The Flatpak manifest lives at `src-tauri/flatpak/app.clustercut.clustercut.yml` and points to a git tag for production builds. The `just flatpak` recipe automatically rewrites the source to use your local working tree, so uncommitted changes are included.

```bash
just flatpak
```

This requires:
- `flatpak-builder`
- `org.gnome.Platform//50` and `org.gnome.Sdk//50` (install from Flathub)
- `org.freedesktop.Sdk.Extension.rust-stable` and `org.freedesktop.Sdk.Extension.node22`

The resulting bundle is exported to `~/Downloads/` by default. Override with:

```bash
just flatpak ~/my-output-dir
```

To run the installed Flatpak:

```bash
just run-flatpak
```

## GNOME Extension

The extension source is in `gnome-extension/`. To build a ZIP for EGO submission:

```bash
just extension-zip
```

To install locally for development (requires GNOME Shell restart — log out/in on Wayland):

```bash
mkdir -p ~/.local/share/gnome-shell/extensions/clustercut@keithvassallo.com
cp -r gnome-extension/* ~/.local/share/gnome-shell/extensions/clustercut@keithvassallo.com/
gnome-extensions enable clustercut@keithvassallo.com
```

## Release Workflow

1. Bump the version:
   ```bash
   just bump-version
   ```

2. Update `CHANGELOG.md` with release notes under an `[Unreleased]` heading.

3. Run the release recipe (syncs version, commits, tags, pushes, builds everything):
   ```bash
   just release
   ```

4. Update the FriendlyHub submission:
   ```bash
   just friendlyhub-update
   ```

## Versioning

`package.json` is the source of truth. Running `npm run sync-version` propagates the version to `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and `src-tauri/flatpak/app.clustercut.clustercut.metainfo.xml`.

## Troubleshooting & Logs

**Linux/macOS:**
```bash
npm run tauri build 2>&1 | tee build.log
```

**Windows (PowerShell):**
```powershell
npm run tauri build *>&1 | Tee-Object build.log
```

**Flatpak runtime logs:**
```bash
flatpak run app.clustercut.clustercut 2>&1 | tee run.log
```
