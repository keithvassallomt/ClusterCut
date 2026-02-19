# Justfile for ClusterCut

# Default: List available commands
default:
    @just --list
    
# Use .env file for all commands
set dotenv-load := true

# Build the native package for the current platform (exe/dmg/deb/rpm)
build:
    npm run tauri build

# Build and verify the Flatpak (from Git source)
flatpak:
    @echo "Generating Cargo sources..."
    python3 src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o src-tauri/flatpak/cargo-sources.json
    @echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node" && python3 -m flatpak_node_generator npm package-lock.json -o src-tauri/flatpak/node-sources.json
    @echo "Building Flatpak bundle from git source and installing..."
    flatpak-builder --user --install --force-clean src-tauri/flatpak/build-dir src-tauri/flatpak/com.keithvassallo.clustercut.yml
    @echo "Exporting bundle from user repo..."
    mkdir -p dist
    VERSION=$(node -p "require('./package.json').version") && flatpak build-bundle ~/.local/share/flatpak/repo dist/ClusterCut_${VERSION}_x86_64.flatpak com.keithvassallo.clustercut
    @echo "Done! Run with: flatpak run com.keithvassallo.clustercut"
    @echo "Bundle created: dist/ClusterCut_$(node -p "require('./package.json').version")_x86_64.flatpak"

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
    rm -f clustercut-extension.zip
    rm -f dist/*.flatpak

# Build the GNOME Extension ZIP
extension-zip:
    @echo "Building GNOME Extension ZIP..."
    rm -f clustercut-extension.zip && cd gnome-extension && zip -r ../clustercut-extension.zip . -x "*.png"
    @echo "Done: clustercut-extension.zip"
