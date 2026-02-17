# Justfile for ClusterCut

# Default: List available commands
default:
    @just --list

# Build the native package for the current platform (exe/dmg/deb/rpm)
build:
    npm run tauri build

# Build, sign, and notarize for macOS (uses .env file for credentials)
# Find your signing identity with: security find-identity -v -p codesigning | grep "Developer ID Application"
build-macos:
    set -a && source .env && set +a && npm run tauri build

# Rebuild the Flatpak (Local bundle method)
flatpak-local:
    @echo "Building native release binary..."
    npm run tauri build
    @echo "Building Flatpak bundle..."
    flatpak-builder --user --install --force-clean src-tauri/flatpak/build-dir src-tauri/flatpak/com.keithvassallo.clustercut.yml
    @echo "Done! Run with: flatpak run com.keithvassallo.clustercut"

# Build for Flathub (Source build)
flatpak-flathub:
    @echo "Generating Cargo sources..."
    python3 src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o src-tauri/flatpak/cargo-sources.json
    @echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node" && python3 -m flatpak_node_generator npm package-lock.json -o src-tauri/flatpak/node-sources.json
    @echo "Building Flatpak from source..."
    flatpak-builder --user --install --force-clean src-tauri/flatpak/build-dir src-tauri/flatpak/com.keithvassallo.clustercut.flathub.yml
    @echo "Done! This version was built locally from source."

# Run the local Flatpak
run-flatpak:
    flatpak run com.keithvassallo.clustercut

# Export the installed Flatpak to a single bundle file
flatpak-bundle:
    @echo "Exporting bundle from user repo..."
    VERSION=$(node -p "require('./package.json').version") && flatpak build-bundle ~/.local/share/flatpak/repo ClusterCut_${VERSION}_x86_64.flatpak com.keithvassallo.clustercut
    @echo "Done: ClusterCut_$(node -p "require('./package.json').version")_x86_64.flatpak"

# Clean all build artifacts
clean:
    rm -rf src-tauri/target
    rm -rf src-tauri/flatpak/build-dir
    rm -rf src-tauri/flatpak/.flatpak-builder
    rm -rf src-tauri/flatpak/shared-modules
    rm -rf src-tauri/flatpak/*.patch

# Setup dependencies for Flatpak build (fetch shared-modules)
setup-flatpak:
    @echo "Cloning shared-modules..."
    git clone https://github.com/flathub/shared-modules.git src-tauri/flatpak/shared-modules 2>/dev/null || echo "shared-modules already exists"
    @echo "Copying necessary patches..."
    # Ensure patches are extracted from shared-modules if not present
    
# Notarize the macOS DMG (requires notarytool-profile in keychain)
notarize:
    #!/usr/bin/env bash
    set -euo pipefail

    DMG_PATH=$(find src-tauri/target/release/bundle/dmg -name "*.dmg" -type f | head -1)

    if [ -z "$DMG_PATH" ]; then
        echo "Error: No DMG found. Run 'just build' first."
        exit 1
    fi

    echo "Notarizing: $DMG_PATH"
    xcrun notarytool submit "$DMG_PATH" --keychain-profile "notarytool-profile" --wait

    echo "Stapling notarization ticket..."
    xcrun stapler staple "$DMG_PATH"

    echo "Verifying notarization..."
    spctl -a -t open --context context:primary-signature -v "$DMG_PATH"

    echo "Done! DMG is notarized and ready for distribution."

# Build the GNOME Extension ZIP
extension-zip:
    @echo "Building GNOME Extension ZIP..."
    cd gnome-extension && zip -r ../clustercut-extension.zip .
    @echo "Done: clustercut-extension.zip"
