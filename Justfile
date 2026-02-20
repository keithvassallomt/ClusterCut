# Justfile for ClusterCut

# Default: List available commands
default:
    @just --list
    
# Use .env file for all commands
set dotenv-load := true

# Build the native package for the current platform (exe/dmg/deb/rpm)
build:
    npm run tauri build

# Generate Flatpak sources and build locally
flatpak flathub_dir="../flathub-clustercut":
    @echo "Generating Cargo sources..."
    python3 src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o {{flathub_dir}}/cargo-sources.json
    @echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node" && python3 -m flatpak_node_generator npm package-lock.json -o {{flathub_dir}}/node-sources.json
    @echo "Building Flatpak bundle from git source and installing..."
    flatpak-builder --user --install --force-clean build-dir {{flathub_dir}}/com.keithvassallo.clustercut.yml
    @echo "Exporting bundle from user repo..."
    mkdir -p dist
    VERSION=$(node -p "require('./package.json').version") && flatpak build-bundle ~/.local/share/flatpak/repo dist/ClusterCut_${VERSION}_x86_64.flatpak com.keithvassallo.clustercut
    @echo "Done! Run with: flatpak run com.keithvassallo.clustercut"
    @echo "Bundle created: dist/ClusterCut_$(node -p "require('./package.json').version")_x86_64.flatpak"

# Build Flatpak and save bundle to ~/Downloads
flatpak-bundle flathub_dir="../flathub-clustercut":
    @echo "Generating Cargo sources..."
    python3 src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o {{flathub_dir}}/cargo-sources.json
    @echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node" && python3 -m flatpak_node_generator npm package-lock.json -o {{flathub_dir}}/node-sources.json
    @echo "Building Flatpak..."
    flatpak-builder --user --install --force-clean build-dir {{flathub_dir}}/com.keithvassallo.clustercut.yml
    @echo "Exporting bundle..."
    VERSION=$(node -p "require('./package.json').version") && \
        flatpak build-bundle ~/.local/share/flatpak/repo ~/Downloads/ClusterCut_${VERSION}_x86_64.flatpak com.keithvassallo.clustercut && \
        echo "Done! Bundle: ~/Downloads/ClusterCut_${VERSION}_x86_64.flatpak"

# Run the local Flatpak
run-flatpak:
    flatpak run com.keithvassallo.clustercut

# Clean all build artifacts
clean:
    rm -rf src-tauri/target
    rm -rf build-dir
    rm -f clustercut-extension.zip
    rm -f dist/*.flatpak
    rm -rf .flatpak-builder

# Build the GNOME Extension ZIP
extension-zip:
    @echo "Building GNOME Extension ZIP..."
    rm -f clustercut-extension.zip && cd gnome-extension && zip -r ../clustercut-extension.zip . -x "*.png"
    @echo "Done: clustercut-extension.zip"
