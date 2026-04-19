# Justfile for ClusterCut

# Default: List available commands
default:
    @just --list
    
# Use .env file for all commands
set dotenv-load := true

# Bump the version: prompts for new version, updates package.json, syncs everywhere
bump-version:
    #!/usr/bin/env bash
    set -euo pipefail
    CURRENT=$(node -p "require('./package.json').version")
    echo "Current version: ${CURRENT}"
    read -rp "New version: " NEW_VERSION
    if [ -z "${NEW_VERSION}" ]; then
        echo "ERROR: No version provided."
        exit 1
    fi
    if ! echo "${NEW_VERSION}" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
        echo "ERROR: Version must be in semver format (e.g. 0.2.0)"
        exit 1
    fi
    if [ "${NEW_VERSION}" = "${CURRENT}" ]; then
        echo "ERROR: New version is the same as current version."
        exit 1
    fi
    # Update package.json
    node -e "
        const fs = require('fs');
        const pkg = JSON.parse(fs.readFileSync('package.json', 'utf-8'));
        pkg.version = '${NEW_VERSION}';
        fs.writeFileSync('package.json', JSON.stringify(pkg, null, 2) + '\n');
    "
    echo "Updated package.json to ${NEW_VERSION}"
    # Sync to all other files
    npm run sync-version
    echo ""
    echo "Version bumped: ${CURRENT} → ${NEW_VERSION}"

# Build the native package for the current platform (exe/dmg/deb/rpm)
build:
    npm run tauri build

# Linux dev setup: install tray icon + GNOME extension into user share dirs (tauri dev only; packaging installs these automatically).
dev-setup:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$(uname -s)" != "Linux" ]; then
        echo "dev-setup is Linux-only; skipping."
        exit 0
    fi

    # 1. Tray icon — Rust/libappindicator looks it up by name from the system icon theme.
    TRAY_DEST="${HOME}/.local/share/icons/hicolor/scalable/status"
    mkdir -p "${TRAY_DEST}"
    install -m 0644 "assets/Tray Icons/svg/clustercut-tray-symbolic.svg" \
        "${TRAY_DEST}/app.clustercut.clustercut-tray-symbolic.svg"
    echo "Installed tray icon to ${TRAY_DEST}/app.clustercut.clustercut-tray-symbolic.svg"

    # 2. GNOME extension — copy (not symlink) so gnome-shell treats it as a normal install.
    EXT_UUID="clustercut@keithvassallo.com"
    EXT_DEST="${HOME}/.local/share/gnome-shell/extensions/${EXT_UUID}"
    mkdir -p "${EXT_DEST}"
    cp -r gnome-extension/. "${EXT_DEST}/"
    echo "Installed GNOME extension to ${EXT_DEST}"

    echo ""
    echo "Next steps:"
    echo "  - Restart the dev app (npm run tauri dev) to pick up the tray icon."
    echo "  - Log out and log back in (GNOME Wayland can't live-reload extensions)."
    echo "    Then: gnome-extensions enable ${EXT_UUID}"

# Build a release: sync version, commit, tag, build native packages (+flatpak on Linux), copy to output dir
release output_dir="~/Downloads":
    #!/usr/bin/env bash
    set -euo pipefail
    OUTPUT_DIR="{{output_dir}}"
    OUTPUT_DIR="${OUTPUT_DIR/#\~/$HOME}"
    mkdir -p "${OUTPUT_DIR}"

    # 1. Sync version
    echo "==> Syncing version..."
    npm run sync-version

    # 2. Read version
    VERSION=$(node -p "require('./package.json').version")
    TAG="v${VERSION}"
    AMEND=false
    if git rev-parse "${TAG}" >/dev/null 2>&1; then
        echo "==> Version: ${VERSION} (tag: ${TAG}) — tag exists, will amend"
        AMEND=true
    else
        echo "==> Version: ${VERSION} (tag: ${TAG})"
    fi

    # 3. Check changelog has notes for this version
    echo "==> Updating CHANGELOG.md..."
    if grep -q '## \[Unreleased\]' CHANGELOG.md; then
        TODAY=$(date +%Y-%m-%d)
        sed -i "s/## \[Unreleased\]/## [${VERSION}] - ${TODAY}/" CHANGELOG.md
    elif ! grep -q "## \[${VERSION}\]" CHANGELOG.md; then
        echo "ERROR: CHANGELOG.md has no [Unreleased] or [${VERSION}] section. Add release notes before releasing."
        exit 1
    fi

    # 4. Update yml tag, commit all changes, tag
    echo "==> Committing release..."
    sed -i "s/tag: v.*/tag: ${TAG}/" src-tauri/flatpak/app.clustercut.clustercut.yml
    sed -i "/^        commit:/d" src-tauri/flatpak/app.clustercut.clustercut.yml
    git add -A
    if [ "${AMEND}" = true ]; then
        git commit --amend -m "v${VERSION}"
        git tag -f "${TAG}"
        echo "==> Amended commit and moved tag ${TAG}"
    else
        git commit -m "v${VERSION}"
        git tag "${TAG}"
        echo "==> Created tag ${TAG}"
    fi

    # 6. Push (must happen before Flatpak build, which clones the tag from GitHub)
    echo "==> Pushing..."
    git push --force-with-lease
    git push origin "${TAG}" --force

    # 7. Build native packages
    echo "==> Building native packages..."
    rm -rf src-tauri/target/release/bundle
    npm run tauri build

    # 8. Copy artifacts to output dir
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

    # 9. Flatpak (Linux only)
    if [ "${OS}" = "Linux" ]; then
        echo "==> Building Flatpak bundle..."
        just flatpak "{{output_dir}}"
    fi

    # 10. Summary
    echo ""
    echo "============================================"
    echo " Release ${TAG} built successfully!"
    echo "============================================"
    echo ""
    echo "Artifacts in ${OUTPUT_DIR}:"
    ls -1 "${OUTPUT_DIR}"/ClusterCut*${VERSION}* 2>/dev/null || echo "  (none found)"
    echo ""
    echo "Done!"

# Clean all build artifacts
clean:
    rm -rf src-tauri/target
    rm -rf build-dir
    rm -f clustercut-extension.zip
    rm -f dist/*.flatpak
    rm -rf .flatpak-builder
    rm -rf .flatpak-staging
    rm -rf .flatpak-shared-modules

# Build the GNOME Extension ZIP, validated by EGO's shexli checker.
extension-zip:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Building GNOME Extension ZIP..."
    rm -f clustercut-extension.zip
    (cd gnome-extension && zip -r ../clustercut-extension.zip . -x "*.png" >/dev/null)

    # EGO now requires extensions pass shexli before publish. Cache the venv so
    # we don't reinstall shexli on every zip build.
    if [ ! -d .venv-shexli ]; then
        echo "==> Creating shexli virtualenv..."
        python3 -m venv .venv-shexli
    fi
    . .venv-shexli/bin/activate
    pip install -q -U shexli

    echo "==> Validating with shexli..."
    if ! shexli clustercut-extension.zip; then
        rm -f clustercut-extension.zip
        echo "ERROR: shexli validation failed; zip removed."
        exit 1
    fi

    echo "Done: clustercut-extension.zip"

# Build and export a local Flatpak bundle for testing
flatpak output_dir="~/Downloads":
    #!/usr/bin/env bash
    set -euo pipefail
    STAGING=".flatpak-staging"
    rm -rf "${STAGING}"
    mkdir -p "${STAGING}"
    cp src-tauri/flatpak/app.clustercut.clustercut.yml "${STAGING}/"
    # Replace git source with local dir so we build from the working tree
    sed -i '/- type: git/{N;N;s/- type: git\n.*url:.*\n.*tag:.*/- type: dir\n        path: '"$(pwd | sed 's/\//\\\//g')"'/}' "${STAGING}/app.clustercut.clustercut.yml"
    # Clone shared-modules if not already cached
    if [ ! -d ".flatpak-shared-modules/libappindicator" ]; then
        echo "Cloning shared-modules..."
        git clone --depth 1 https://github.com/flathub/shared-modules.git .flatpak-shared-modules
    fi
    ln -s "$(pwd)/.flatpak-shared-modules" "${STAGING}/shared-modules"
    # Generate sources into staging
    echo "Generating Cargo sources..."
    python3 src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o "${STAGING}/cargo-sources.json"
    echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node"
    python3 -m flatpak_node_generator npm package-lock.json -o "${STAGING}/node-sources.json"
    # Build and install
    echo "Building Flatpak..."
    flatpak-builder --user --install --force-clean build-dir "${STAGING}/app.clustercut.clustercut.yml"
    # Export bundle
    OUTPUT_DIR="{{output_dir}}"
    OUTPUT_DIR="${OUTPUT_DIR/#\~/$HOME}"
    mkdir -p "${OUTPUT_DIR}"
    VERSION=$(node -p "require('./package.json').version")
    flatpak build-bundle ~/.local/share/flatpak/repo "${OUTPUT_DIR}/ClusterCut_${VERSION}_x86_64.flatpak" app.clustercut.clustercut
    echo "Done! Bundle: ${OUTPUT_DIR}/ClusterCut_${VERSION}_x86_64.flatpak"
    echo "Run with: flatpak run app.clustercut.clustercut"

# Run the local Flatpak
run-flatpak:
    flatpak run app.clustercut.clustercut

# Prepare FriendlyHub submission: update manifest, regenerate sources, copy to submission dir
friendlyhub-update submission_dir="/home/keith/LocalCode/keithvassallomt/app.clustercut.ClusterCut":
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(node -p "require('./package.json').version")
    TAG="v${VERSION}"
    echo "Preparing FriendlyHub submission for ${TAG}..."
    # Verify the upstream tag exists
    if ! git rev-parse "${TAG}" >/dev/null 2>&1; then
        echo "ERROR: Tag ${TAG} does not exist. Tag and push the upstream release first."
        exit 1
    fi
    echo "Tag ${TAG} found."
    # Verify the release has a description in metainfo
    METAINFO="src-tauri/flatpak/app.clustercut.clustercut.metainfo.xml"
    if ! grep -A2 "version=\"${VERSION}\"" "${METAINFO}" | grep -q "<description>"; then
        echo "ERROR: Release ${VERSION} in ${METAINFO} has no <description>. Add release notes before updating."
        exit 1
    fi
    # Create submission directory if needed
    mkdir -p "{{submission_dir}}"
    # Copy and update the yml with current tag and commit
    YML="{{submission_dir}}/app.clustercut.clustercut.yml"
    cp src-tauri/flatpak/app.clustercut.clustercut.yml "${YML}"
    echo "Updating yml tag..."
    sed -i "s/tag: v.*/tag: ${TAG}/" "${YML}"
    # Update the template in-repo as well
    sed -i "s/tag: v.*/tag: ${TAG}/" src-tauri/flatpak/app.clustercut.clustercut.yml
    # Copy the metainfo manifest
    echo "Copying metainfo manifest..."
    cp "${METAINFO}" "{{submission_dir}}/"
    # Generate sources into submission dir
    echo "Generating Cargo sources..."
    python3 src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o "{{submission_dir}}/cargo-sources.json"
    echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node"
    python3 -m flatpak_node_generator npm package-lock.json -o "{{submission_dir}}/node-sources.json"
    echo ""
    echo "============================================"
    echo " FriendlyHub submission prepared!"
    echo "============================================"
    echo ""
    echo "Submission directory: {{submission_dir}}"
    echo "Contents:"
    ls -1 "{{submission_dir}}/"
