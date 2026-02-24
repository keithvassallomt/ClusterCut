# Justfile for ClusterCut

# Default: List available commands
default:
    @just --list
    
# Use .env file for all commands
set dotenv-load := true

# Build the native package for the current platform (exe/dmg/deb/rpm)
build:
    npm run tauri build

# Build a release: sync version, commit, tag, build native packages (+flatpak on Linux), copy to output dir
release output_dir="~/Downloads" flathub_dir="../flathub-clustercut":
    #!/usr/bin/env bash
    set -euo pipefail
    OUTPUT_DIR="{{output_dir}}"
    OUTPUT_DIR="${OUTPUT_DIR/#\~/$HOME}"
    mkdir -p "${OUTPUT_DIR}"

    # 1. Sync version
    echo "==> Syncing version..."
    npm run sync-version

    # 2. Read version and check tag doesn't exist
    VERSION=$(node -p "require('./package.json').version")
    TAG="v${VERSION}"
    echo "==> Version: ${VERSION} (tag: ${TAG})"
    if git rev-parse "${TAG}" >/dev/null 2>&1; then
        echo "ERROR: Tag ${TAG} already exists."
        exit 1
    fi

    # 3. Commit all changes and tag
    echo "==> Committing release..."
    git add -u
    git commit -m "v${VERSION}"
    git tag "${TAG}"
    echo "==> Created tag ${TAG}"

    # 4. Build native packages
    echo "==> Building native packages..."
    npm run tauri build

    # 5. Copy artifacts to output dir
    OS="$(uname -s)"
    echo "==> Copying artifacts to ${OUTPUT_DIR}..."
    case "${OS}" in
        Linux)
            cp src-tauri/target/release/bundle/deb/*.deb "${OUTPUT_DIR}/" 2>/dev/null || true
            cp src-tauri/target/release/bundle/rpm/*.rpm "${OUTPUT_DIR}/" 2>/dev/null || true
            ;;
        Darwin)
            cp src-tauri/target/release/bundle/dmg/*.dmg "${OUTPUT_DIR}/" 2>/dev/null || true
            ;;
        MINGW*|MSYS*|CYGWIN*)
            cp src-tauri/target/release/bundle/nsis/*.exe "${OUTPUT_DIR}/" 2>/dev/null || true
            ;;
    esac

    # 6. Flatpak (Linux only)
    if [ "${OS}" = "Linux" ]; then
        echo "==> Building Flatpak bundle..."
        just flatpak "{{flathub_dir}}" "{{output_dir}}"
    fi

    # 7. Summary
    echo ""
    echo "============================================"
    echo " Release ${TAG} built successfully!"
    echo "============================================"
    echo ""
    echo "Artifacts in ${OUTPUT_DIR}:"
    ls -1 "${OUTPUT_DIR}"/ClusterCut*${VERSION}* 2>/dev/null || echo "  (none found)"
    echo ""
    echo "Pushing..."
    git push
    git push origin "${TAG}"
    echo "Done!"

# Clean all build artifacts
clean:
    rm -rf src-tauri/target
    rm -rf build-dir
    rm -f clustercut-extension.zip
    rm -f dist/*.flatpak
    rm -rf .flatpak-builder
    rm -rf .flatpak-staging

# Build the GNOME Extension ZIP
extension-zip:
    @echo "Building GNOME Extension ZIP..."
    rm -f clustercut-extension.zip && cd gnome-extension && zip -r ../clustercut-extension.zip . -x "*.png"
    @echo "Done: clustercut-extension.zip"

# Build and export a local Flatpak bundle for testing (does not modify flathub repo)
flatpak flathub_dir="../flathub-clustercut" output_dir="~/Downloads":
    #!/usr/bin/env bash
    set -euo pipefail
    # Stage in a temp dir so we don't modify the flathub repo
    STAGING=".flatpak-staging"
    rm -rf "${STAGING}"
    mkdir -p "${STAGING}"
    cp {{flathub_dir}}/com.keithvassallo.clustercut.yml "${STAGING}/"
    ln -s "$(cd {{flathub_dir}} && pwd)/shared-modules" "${STAGING}/shared-modules"
    # Generate sources into staging
    echo "Generating Cargo sources..."
    python3 src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o "${STAGING}/cargo-sources.json"
    echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node"
    python3 -m flatpak_node_generator npm package-lock.json -o "${STAGING}/node-sources.json"
    # Build and install
    echo "Building Flatpak..."
    flatpak-builder --user --install --force-clean build-dir "${STAGING}/com.keithvassallo.clustercut.yml"
    # Export bundle
    OUTPUT_DIR="{{output_dir}}"
    OUTPUT_DIR="${OUTPUT_DIR/#\~/$HOME}"
    mkdir -p "${OUTPUT_DIR}"
    VERSION=$(node -p "require('./package.json').version")
    flatpak build-bundle ~/.local/share/flatpak/repo "${OUTPUT_DIR}/ClusterCut_${VERSION}_x86_64.flatpak" com.keithvassallo.clustercut
    echo "Done! Bundle: ${OUTPUT_DIR}/ClusterCut_${VERSION}_x86_64.flatpak"
    echo "Run with: flatpak run com.keithvassallo.clustercut"

# Run the local Flatpak
run-flatpak:
    flatpak run com.keithvassallo.clustercut

# Prepare Flathub repo for a new release: branch, update manifest, regenerate sources, build
flathub-update flathub_dir="../flathub-clustercut":
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(node -p "require('./package.json').version")
    TAG="v${VERSION}"
    echo "Preparing Flathub update for ${TAG}..."
    # Verify the upstream tag exists
    if ! git rev-parse "${TAG}" >/dev/null 2>&1; then
        echo "ERROR: Tag ${TAG} does not exist. Tag and push the upstream release first."
        exit 1
    fi
    COMMIT=$(git rev-parse "${TAG}")
    echo "Tag ${TAG} -> commit ${COMMIT}"
    # Verify the release has a description in metainfo
    METAINFO="src-tauri/flatpak/com.keithvassallo.clustercut.metainfo.xml"
    if ! grep -A2 "version=\"${VERSION}\"" "${METAINFO}" | grep -q "<description>"; then
        echo "ERROR: Release ${VERSION} in ${METAINFO} has no <description>. Add release notes before updating Flathub."
        exit 1
    fi
    # Create update branch in flathub repo
    BRANCH="update/${TAG}"
    echo "Creating branch ${BRANCH} in {{flathub_dir}}..."
    git -C {{flathub_dir}} checkout master
    git -C {{flathub_dir}} pull
    git -C {{flathub_dir}} checkout -b "${BRANCH}"
    # Update manifest tag and commit
    MANIFEST="{{flathub_dir}}/com.keithvassallo.clustercut.yml"
    echo "Updating manifest tag and commit..."
    sed -i "s/tag: v.*/tag: ${TAG}/" "${MANIFEST}"
    sed -i "s/commit: .*/commit: ${COMMIT}/" "${MANIFEST}"
    # Regenerate sources
    echo "Generating Cargo sources..."
    python3 src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o {{flathub_dir}}/cargo-sources.json
    echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node"
    python3 -m flatpak_node_generator npm package-lock.json -o {{flathub_dir}}/node-sources.json
    # Build locally to verify
    echo "Building Flatpak locally to verify..."
    flatpak-builder --user --install --force-clean build-dir {{flathub_dir}}/com.keithvassallo.clustercut.yml
    echo ""
    echo "============================================"
    echo " Flathub update prepared successfully!"
    echo "============================================"
    echo ""
    echo "Next steps:"
    echo "  1. Review changes:  cd {{flathub_dir}} && git diff"
    echo "  2. Commit:          git -C {{flathub_dir}} add -A && git -C {{flathub_dir}} commit -m 'Update to ${TAG}'"
    echo "  3. Push:            git -C {{flathub_dir}} push -u origin ${BRANCH}"
    echo "  4. Open a PR targeting 'master' at:"
    echo "     https://github.com/flathub/com.keithvassallo.clustercut/compare/master...${BRANCH}"
