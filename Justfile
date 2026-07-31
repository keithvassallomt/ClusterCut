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

# Build a release binary, skipping installer bundles (deb/rpm/dmg/exe/AppImage). Output: src-tauri/target/release/clustercut
build-no-bundle:
    npm run tauri build -- --no-bundle

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

    # Cross-platform in-place sed (BSD sed on macOS requires an extension arg after -i)
    sedi() { if [[ "$(uname -s)" == "Darwin" ]]; then sed -i '' "$@"; else sed -i "$@"; fi; }

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
        sedi "s/## \[Unreleased\]/## [${VERSION}] - ${TODAY}/" CHANGELOG.md
    elif ! grep -q "## \[${VERSION}\]" CHANGELOG.md; then
        echo "ERROR: CHANGELOG.md has no [Unreleased] or [${VERSION}] section. Add release notes before releasing."
        exit 1
    fi

    # 4. Update yml tag, commit all changes, tag
    echo "==> Committing release..."
    sedi "s/tag: v.*/tag: ${TAG}/" src-tauri/flatpak/app.clustercut.clustercut.yml
    sedi "/^        commit:/d" src-tauri/flatpak/app.clustercut.clustercut.yml
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

    # 10. Create (or update) the GitHub release and upload THIS platform's artifacts.
    # The release is published immediately rather than kept as a draft: assets on a
    # draft release are not publicly downloadable, and the AUR package's source URL
    # has to resolve the moment it is pushed. So the release goes live with whatever
    # has been built so far, and the macOS/Windows runs add their installers when
    # they happen.
    # Notes = packaging/release-notes-template.md with @VERSION@ substituted and
    # @CHANGELOG@ replaced by this version's CHANGELOG.md section, so every release
    # keeps the same shape (install instructions, tips, Full Changelog footer).
    echo "==> Publishing GitHub release ${TAG}..."
    CHANGELOG_SECTION=$(mktemp)
    awk -v ver="${VERSION}" '
        $0 ~ "^## \\[" ver "\\]" { inside = 1; next }
        inside && /^## \[/ { exit }
        inside {
            # Drop the blank line that follows the version heading; the template
            # already spaces the changelog away from the title.
            if (!started && $0 ~ /^[[:space:]]*$/) next
            started = 1
            print
        }
    ' CHANGELOG.md > "${CHANGELOG_SECTION}"
    if [ ! -s "${CHANGELOG_SECTION}" ]; then
        echo "ERROR: no CHANGELOG.md section for ${VERSION} to use as release notes."
        exit 1
    fi
    NOTES_FILE=$(mktemp)
    awk -v ver="${VERSION}" -v sect="${CHANGELOG_SECTION}" '
        /@CHANGELOG@/ { while ((getline line < sect) > 0) print line; next }
        { gsub(/@VERSION@/, ver); print }
    ' packaging/release-notes-template.md > "${NOTES_FILE}"
    rm -f "${CHANGELOG_SECTION}"
    if gh release view "${TAG}" >/dev/null 2>&1; then
        gh release edit "${TAG}" --notes-file "${NOTES_FILE}" >/dev/null
        echo "    updated existing release"
    else
        gh release create "${TAG}" --title "${TAG}" --notes-file "${NOTES_FILE}" >/dev/null
        echo "    created release"
    fi
    rm -f "${NOTES_FILE}"

    # No bash arrays here on purpose: macOS ships bash 3.2, where expanding an
    # empty array under `set -u` is an error. Unmatched globs fall through as
    # literals, which the -f test filters out.
    UPLOADED=0
    upload_if_present() {
        for f in "$@"; do
            if [ -f "${f}" ]; then
                echo "    uploading $(basename "${f}")..."
                gh release upload "${TAG}" "${f}" --clobber
                UPLOADED=$((UPLOADED + 1))
            fi
        done
    }
    case "${OS}" in
        Linux)
            upload_if_present src-tauri/target/release/bundle/deb/*.deb \
                              src-tauri/target/release/bundle/rpm/*.rpm \
                              "${OUTPUT_DIR}/ClusterCut_${VERSION}_x86_64.flatpak"
            ;;
        Darwin)
            upload_if_present src-tauri/target/release/bundle/dmg/*.dmg
            ;;
        MINGW*|MSYS*|CYGWIN*)
            upload_if_present src-tauri/target/release/bundle/nsis/*.exe
            ;;
    esac
    if [ "${UPLOADED}" -eq 0 ]; then
        echo "WARNING: no ${OS} artifacts found to upload."
    fi

    # 11. AUR (Linux only — needs the .deb to be on the release, which step 10 just
    # did). Non-fatal: a missing AUR setup must not fail an otherwise good release.
    if [ "${OS}" = "Linux" ]; then
        echo "==> Publishing to the AUR..."
        just aur-publish || echo "WARNING: AUR publish failed (see above). Fix it and run 'just aur-publish'."
    fi

    # 12. Summary
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
    rm -f clustercut@keithvassallo.com.shell-extension.zip
    rm -f dist/*.flatpak
    rm -rf .flatpak-builder
    rm -rf .flatpak-staging
    rm -rf .flatpak-shared-modules

# Build the GNOME Extension ZIP, validated by EGO's shexli checker.
extension-zip:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "Building GNOME Extension ZIP..."
    rm -f clustercut@keithvassallo.com.shell-extension.zip
    (cd gnome-extension && zip -r ../clustercut@keithvassallo.com.shell-extension.zip . -x "*.png" >/dev/null)

    # EGO now requires extensions pass shexli before publish. Cache the venv so
    # we don't reinstall shexli on every zip build.
    if [ ! -d .venv-shexli ]; then
        echo "==> Creating shexli virtualenv..."
        python3 -m venv .venv-shexli
    fi
    . .venv-shexli/bin/activate
    pip install -q -U shexli

    echo "==> Validating with shexli..."
    if ! shexli clustercut@keithvassallo.com.shell-extension.zip; then
        rm -f clustercut@keithvassallo.com.shell-extension.zip
        echo "ERROR: shexli validation failed; zip removed."
        exit 1
    fi

    echo "Done: clustercut@keithvassallo.com.shell-extension.zip"

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
    # Ensure the Flatpak runtime + SDK extensions the manifest needs are installed.
    # Branch pins: GNOME 50 is built on freedesktop 25.08, which is the branch the
    # Sdk.Extension.* bundles are published at (NOT branch 50).
    for ref in \
        "org.gnome.Platform//50" \
        "org.gnome.Sdk//50" \
        "org.freedesktop.Sdk.Extension.rust-stable//25.08" \
        "org.freedesktop.Sdk.Extension.node22//25.08"; do
        if ! flatpak info --user "${ref}" >/dev/null 2>&1 \
            && ! flatpak info --system "${ref}" >/dev/null 2>&1; then
            echo "Installing missing Flatpak: ${ref}..."
            flatpak install --user -y flathub "${ref}"
        fi
    done
    # Generator scripts need aiohttp / PyYAML / tomlkit — install into a cached venv.
    if [ ! -d .venv-flatpak ]; then
        echo "Creating flatpak tooling virtualenv..."
        python3 -m venv .venv-flatpak
        .venv-flatpak/bin/pip install -q -U pip aiohttp PyYAML tomlkit
    fi
    PY=.venv-flatpak/bin/python3
    # Generate sources into staging
    echo "Generating Cargo sources..."
    "${PY}" src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o "${STAGING}/cargo-sources.json"
    echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node"
    "${PY}" -m flatpak_node_generator npm package-lock.json -o "${STAGING}/node-sources.json"
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
    # Verify the release has a description in metainfo. The previous
    # grep -A2 check was scoped to "any <description> within 2 lines", which
    # silently inherited the next-older release's notes when the current
    # entry was a self-closing <release ... /> — so missing notes slipped
    # through unnoticed. xmllint scopes the check to the actual release node.
    METAINFO="src-tauri/flatpak/app.clustercut.clustercut.metainfo.xml"
    if ! command -v xmllint >/dev/null 2>&1; then
        echo "ERROR: xmllint (libxml2) is required to verify release notes. Install libxml2 and re-run."
        exit 1
    fi
    DESC_COUNT=$(xmllint --xpath "count(//release[@version=\"${VERSION}\"]/description)" "${METAINFO}" 2>/dev/null || echo 0)
    if [ "${DESC_COUNT}" != "1" ]; then
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
    # Generator scripts need aiohttp / PyYAML / tomlkit — install into a cached venv.
    if [ ! -d .venv-flatpak ]; then
        echo "Creating flatpak tooling virtualenv..."
        python3 -m venv .venv-flatpak
        .venv-flatpak/bin/pip install -q -U pip aiohttp PyYAML tomlkit
    fi
    PY=.venv-flatpak/bin/python3
    # Generate sources into submission dir
    echo "Generating Cargo sources..."
    "${PY}" src-tauri/flatpak/builder-tools/cargo/flatpak-cargo-generator.py src-tauri/Cargo.lock -o "{{submission_dir}}/cargo-sources.json"
    echo "Generating Node sources..."
    export PYTHONPATH="${PYTHONPATH:-}:$(pwd)/src-tauri/flatpak/builder-tools/node"
    "${PY}" -m flatpak_node_generator npm package-lock.json -o "{{submission_dir}}/node-sources.json"
    echo ""
    echo "============================================"
    echo " FriendlyHub submission prepared!"
    echo "============================================"
    echo ""
    echo "Submission directory: {{submission_dir}}"
    echo "Contents:"
    ls -1 "{{submission_dir}}/"

# ── AUR (clustercut-bin) ──────────────────────────────────────────────────────
# Repackages the published amd64 .deb, so the release must already be tagged and
# its .deb uploaded to the GitHub release before either recipe will work.

# Render + validate the AUR package for the current version. Output: <output_dir>/clustercut-bin-<version>/
aur output_dir="~/Downloads":
    #!/usr/bin/env bash
    set -euo pipefail
    OUTPUT_DIR="{{output_dir}}"
    OUTPUT_DIR="${OUTPUT_DIR/#\~/$HOME}"

    VERSION=$(node -p "require('./package.json').version")
    STAGING=$(mktemp -d)
    trap 'rm -rf "${STAGING}"' EXIT

    just _aur-stage "${STAGING}"

    echo "==> Building package with makepkg (validates the PKGBUILD)..."
    (cd "${STAGING}" && makepkg -f --noconfirm --nocheck)

    DEST="${OUTPUT_DIR}/clustercut-bin-${VERSION}"
    mkdir -p "${DEST}"
    cp "${STAGING}/PKGBUILD" "${STAGING}/.SRCINFO" "${DEST}/"
    cp "${STAGING}"/*.pkg.tar.zst "${DEST}/"

    echo ""
    echo "============================================"
    echo " AUR package for ${VERSION} built"
    echo "============================================"
    echo ""
    echo "${DEST}:"
    ls -1 "${DEST}"
    echo ""
    echo "Install locally:  sudo pacman -U ${DEST}/*.pkg.tar.zst"
    echo "Publish to AUR:   just aur-publish"

# Render, validate, and push the current version to the AUR. Needs an AUR account with your SSH key.
aur-publish:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION=$(node -p "require('./package.json').version")
    AUR_REPO="ssh://aur@aur.archlinux.org/clustercut-bin.git"

    STAGING=$(mktemp -d)
    CHECKOUT=$(mktemp -d)
    trap 'rm -rf "${STAGING}" "${CHECKOUT}"' EXIT

    just _aur-stage "${STAGING}"

    # Validate before publishing — a broken PKGBUILD on the AUR is public.
    echo "==> Validating with makepkg..."
    (cd "${STAGING}" && makepkg -f --noconfirm --nocheck >/dev/null)
    echo "==> Package builds cleanly."

    echo "==> Cloning ${AUR_REPO}..."
    if ! git clone --quiet "${AUR_REPO}" "${CHECKOUT}" 2>/dev/null; then
        echo ""
        echo "ERROR: could not reach the AUR over SSH. One-time setup:"
        echo "  1. Create an account at https://aur.archlinux.org/register"
        echo "  2. Add your public key (~/.ssh/id_ed25519.pub) under My Account -> SSH Public Key"
        echo "  3. Add to ~/.ssh/config:"
        echo "       Host aur.archlinux.org"
        echo "         User aur"
        echo "         IdentityFile ~/.ssh/id_ed25519"
        exit 1
    fi

    cp "${STAGING}/PKGBUILD" "${STAGING}/.SRCINFO" "${CHECKOUT}/"
    cd "${CHECKOUT}"
    if git diff --quiet && git diff --cached --quiet; then
        echo "==> AUR already up to date at ${VERSION}; nothing to push."
        exit 0
    fi
    git add PKGBUILD .SRCINFO
    git commit --quiet -m "clustercut-bin ${VERSION}"
    git push --quiet origin master
    echo ""
    echo "==> Published clustercut-bin ${VERSION} to the AUR."
    echo "    https://aur.archlinux.org/packages/clustercut-bin"

# Internal: render PKGBUILD + .SRCINFO for the current version into <staging_dir>
_aur-stage staging_dir:
    #!/usr/bin/env bash
    set -euo pipefail
    STAGING="{{staging_dir}}"

    if [ "$(uname -s)" != "Linux" ] || ! command -v makepkg >/dev/null 2>&1; then
        echo "ERROR: the AUR recipes need an Arch host with base-devel (makepkg not found)."
        exit 1
    fi

    VERSION=$(node -p "require('./package.json').version")
    TAG="v${VERSION}"
    DEB="ClusterCut_${VERSION}_amd64.deb"
    URL="https://github.com/keithvassallomt/ClusterCut/releases/download/${TAG}/${DEB}"

    # The PKGBUILD points at the release asset, so it has to exist before we can
    # checksum it — catch that here rather than emitting a broken PKGBUILD.
    echo "==> Checking ${TAG} for ${DEB}..."
    if ! gh release view "${TAG}" --json assets --jq '.assets[].name' 2>/dev/null | grep -qx "${DEB}"; then
        echo "ERROR: ${DEB} is not attached to release ${TAG}."
        echo "       Run 'just release' and upload the artifacts first:"
        echo "       gh release upload ${TAG} ~/Downloads/${DEB}"
        exit 1
    fi

    echo "==> Downloading ${DEB} to checksum it..."
    curl -sSfL -o "${STAGING}/${DEB}" "${URL}"
    SHA256=$(sha256sum "${STAGING}/${DEB}" | cut -d' ' -f1)
    echo "    sha256: ${SHA256}"

    sed -e "s/@PKGVER@/${VERSION}/g" -e "s/@SHA256@/${SHA256}/g" \
        packaging/aur/PKGBUILD.in > "${STAGING}/PKGBUILD"

    # makepkg reuses the already-downloaded deb instead of fetching it again.
    (cd "${STAGING}" && makepkg --printsrcinfo > .SRCINFO)
    echo "==> Rendered PKGBUILD + .SRCINFO for ${VERSION}"
