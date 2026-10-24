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
| `just bump-version` | Bump the version across all project files (prompts, or `just bump-version 1.2.3`) |
| `just release` | Cut a release: checks, tag and push. GitHub builds every installer and publishes the release; then the AUR, the FriendlyHub files and the EGO zip (see [Release Workflow](#release-workflow)) |
| `just release-finish X.Y.Z` | The part of `just release` after the push; run it again after fixing a failed workflow |
| `just release-dry-run` | Run the release workflow on GitHub, publishing nothing |
| `just aur` | Build + validate the `clustercut-bin` AUR package locally (Arch host) |
| `just aur-publish` | Push the current version's AUR package to the AUR |
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

Releases are cut from `main` on an Arch machine (the AUR step needs `makepkg`). Pushing a `v*` tag runs [`.github/workflows/release.yml`](.github/workflows/release.yml), which builds every installer and publishes the GitHub release.

### Before releasing

- `CHANGELOG.md`: the release's changes under `[Unreleased]` (renamed to `[X.Y.Z] - today` at release) or under `[X.Y.Z]`.
- `src-tauri/flatpak/app.clustercut.clustercut.metainfo.xml`: a `<release version="X.Y.Z">` with a `<description>`, the short "What's new" that FriendlyHub and software centres show. The release sets its date.
- If `gnome-extension/` changed since the last release: a new `version-name` in its `metadata.json`.

### `just release`

1. **Checks**, before anything changes: on `main`, nothing uncommitted, not behind `origin/main`, and the macOS signing secrets exist.
2. **Version**: asks for it, suggesting `package.json`'s when that is already ahead of the last tag. It must be newer than the last release, and not tagged yet.
3. **Changelog and metainfo**: the notes above exist, and the metainfo passes `appstreamcli validate`.
4. **GNOME extension**, if it changed since the last release: its `version-name` changed, and the zip passes EGO's `shexli` checks.
5. **Tests**: `cargo test`, and the frontend builds.
6. **Confirm**: sets the version everywhere, dates the changelog and the metainfo release, points the Flatpak manifest at the new tag, then shows the release notes and the changed files and asks. Answering no puts every file back.
7. **Tag**: commits `vX.Y.Z`, tags it, and pushes both, which starts the workflow.
8. **Finish** (`just release-finish X.Y.Z`): waits for the workflow, publishes `clustercut-bin` to the AUR, and saves to `~/Downloads`:
   - `ClusterCut-X.Y.Z-friendlyhub/`: the manifest (pinned to the tag and its commit), the metainfo, and the cargo and node sources. The workflow built the Flatpak from these same files.
   - `clustercut@keithvassallo.com.shell-extension.zip`, only when the extension changed since the previous release.

Then open the FriendlyHub update PR with the four files, and upload the zip to EGO.

### What the workflow builds

| Job | Output |
| :--- | :--- |
| deb and rpm | `ClusterCut_X.Y.Z_amd64.deb` and `ClusterCut-X.Y.Z-1.x86_64.rpm`, built on Ubuntu 22.04 so they run with glibc 2.35 and newer |
| Windows installer | `ClusterCut_X.Y.Z_x64-setup.exe` |
| macOS DMG | `ClusterCut_X.Y.Z_aarch64.dmg`, signed and notarized |
| FriendlyHub files | The submission that `release-finish` saves |
| Flatpak bundle | `ClusterCut_X.Y.Z_x86_64.flatpak`, built from the FriendlyHub files |
| GitHub release | The five installers. Notes: `packaging/release-notes-template.md` with the version's `CHANGELOG.md` section |

The GitHub release is only created once every build has passed.

`just release-dry-run` runs the same builds from `main` (the DMG signed and notarized too) and publishes nothing; the installers and the FriendlyHub files are on the run's page.

### If the workflow fails

The tag is pushed by then, but nothing is published. For a flaky job, rerun it, then finish:

```bash
gh run rerun <run-id> --failed
just release-finish X.Y.Z
```

To fix something and release the same version again, delete the tag, commit the fix, and run `just release` again:

```bash
git push origin --delete vX.Y.Z && git tag -d vX.Y.Z
```

If the macOS job fails while laying out the DMG window (a Finder or AppleScript error), remove `TAURI_BUNDLER_DMG_IGNORE_CI` from the workflow. The DMG then opens without its background and icon layout.

### One-time setup

**macOS signing.** The workflow signs and notarizes the DMG with the Developer ID that the Mac's `.env` uses, stored as repository secrets. On the Mac:

1. In Keychain Access, under My Certificates, export "Developer ID Application: ..." (the certificate with its private key) as a `.p12`, with a password.
2. Store the secrets. Each `gh secret set` without a value asks for it.
   ```bash
   base64 -i DeveloperID.p12 | gh secret set APPLE_CERTIFICATE -R keithvassallomt/ClusterCut
   gh secret set APPLE_CERTIFICATE_PASSWORD -R keithvassallomt/ClusterCut  # the .p12's password
   gh secret set APPLE_SIGNING_IDENTITY -R keithvassallomt/ClusterCut      # Developer ID Application: Name (TEAMID)
   gh secret set APPLE_ID -R keithvassallomt/ClusterCut
   gh secret set APPLE_PASSWORD -R keithvassallomt/ClusterCut              # an app-specific password
   gh secret set APPLE_TEAM_ID -R keithvassallomt/ClusterCut
   ```
   The last four take the same values as in `.env`.
3. Delete the `.p12`.

**AUR.** An AUR account with the release machine's SSH key; `just aur-publish` prints the steps when it can't connect.

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
