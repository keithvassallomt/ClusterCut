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
    # (For now, we assume they are checked in or handled by the repo structure, 
    # but strictly speaking strict Flathub builds would re-download everything.
    # We will rely on the repo state for local builds for now.)
