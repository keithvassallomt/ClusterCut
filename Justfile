# Justfile for ClusterCut

# Default: List available commands
default:
    @just --list

# Build the native package for the current platform (exe/dmg/deb/rpm)
build:
    npm run tauri build

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
# Build the GNOME Extension ZIP
extension-zip:
    @echo "Building GNOME Extension ZIP..."
    cd gnome-extension && zip -r ../clustercut-extension.zip .
    @echo "Done: clustercut-extension.zip"
