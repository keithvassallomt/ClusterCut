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
| `just release` | Full release workflow: version sync, commit, tag, push, build native + Flatpak, publish the GitHub release, push to the AUR |
| `just aur` | Build + validate the `clustercut-bin` AUR package locally (Arch host) |
| `just aur-publish` | Push the current version's AUR package to the AUR |
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

   Run it **on Linux first**. That run creates the GitHub release — notes taken from the
   `CHANGELOG.md` section for this version — uploads the `.deb`, `.rpm` and `.flatpak`,
   and then pushes `clustercut-bin` to the AUR. Running it later on macOS and Windows
   adds the `.dmg` and `.exe` to the same release.

   The release is published straight away rather than held as a draft, because assets on
   a draft release are not publicly downloadable and the AUR package's source URL points
   at the `.deb`. So the release is briefly live with only the Linux installers on it.

   The AUR step needs an Arch host with `base-devel`, `just`, and an AUR account whose
   SSH key is configured (see `just aur-publish` for the setup it prints). It is
   non-fatal: if it fails, the release still completes and `just aur-publish` can be
   re-run on its own.

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
