# Justfile for ClusterCut

# Default: List available commands
default:
    @just --list
    
# Use .env file for all commands
set dotenv-load := true

# Bump the version: updates package.json (prompting when no version is given), syncs everywhere
bump-version version="":
    #!/usr/bin/env bash
    set -euo pipefail
    CURRENT=$(node -p "require('./package.json').version")
    NEW_VERSION="{{version}}"
    if [ -z "${NEW_VERSION}" ]; then
        echo "Current version: ${CURRENT}"
        read -rp "New version: " NEW_VERSION
    fi
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

    # 2. Launcher + icon under the app ID. Desktop portals only accept
    #    Registry.Register("app.clustercut.clustercut") when a matching .desktop
    #    exists; otherwise they guess the app ID from the launching process and
    #    global shortcuts register under the wrong app.
    APPS_DEST="${HOME}/.local/share/applications"
    ICON_DEST="${HOME}/.local/share/icons/hicolor/scalable/apps"
    mkdir -p "${APPS_DEST}" "${ICON_DEST}"
    install -m 0644 src-tauri/flatpak/app.clustercut.clustercut.desktop "${APPS_DEST}/"
    install -m 0644 src-tauri/flatpak/app.clustercut.clustercut.svg "${ICON_DEST}/"
    echo "Installed launcher to ${APPS_DEST}/app.clustercut.clustercut.desktop"

    # 3. GNOME extension — copy (not symlink) so gnome-shell treats it as a normal install.
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

# From main on the Arch machine. Checks the changelog, the metainfo notes and (if it
# changed) the GNOME extension, then tags and pushes; GitHub builds every installer
# and publishes the release, then release-finish runs. See BUILD.md, "Release Workflow".
# Cut a release: asks for the version, checks, tags; GitHub builds and publishes
release output_dir="~/Downloads":
    #!/usr/bin/env bash
    set -euo pipefail
    die() { echo "ERROR: $*" >&2; exit 1; }
    REPO=keithvassallomt/ClusterCut
    METAINFO=src-tauri/flatpak/app.clustercut.clustercut.metainfo.xml
    MANIFEST=src-tauri/flatpak/app.clustercut.clustercut.yml

    # Preflight: nothing changes until all of these pass.
    command -v makepkg >/dev/null || die "release from an Arch machine: the AUR step needs makepkg"
    for tool in gh node xmllint appstreamcli zip; do
        command -v "${tool}" >/dev/null || die "${tool} is not installed"
    done
    [ "$(git branch --show-current)" = main ] || die "release from main"
    [ -z "$(git status --porcelain)" ] || die "commit or stash your changes first"
    git fetch -q --tags origin || die "fetching from origin failed; run 'git fetch --tags origin' to see why"
    git merge-base --is-ancestor origin/main HEAD || die "main is behind origin/main; pull first"
    SECRETS=$(gh secret list -R "${REPO}" --json name --jq '.[].name')
    for secret in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
        grep -qx "${secret}" <<< "${SECRETS}" || die "the ${secret} GitHub secret is missing (macOS signing, see BUILD.md)"
    done

    # 1. The version: newer than the last release, and not tagged yet.
    LAST_TAG=$(git tag --list 'v[0-9]*' --sort=-v:refname | head -1)
    LAST="${LAST_TAG#v}"
    CURRENT=$(node -p "require('./package.json').version")
    newer() { [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" = "$1" ]; }
    SUGGESTED=""
    if newer "${CURRENT}" "${LAST}"; then
        SUGGESTED="${CURRENT}"
    fi
    echo "Last release: ${LAST_TAG}. package.json: ${CURRENT}."
    read -rp "Version to release${SUGGESTED:+ [${SUGGESTED}]}: " VERSION
    VERSION="${VERSION:-${SUGGESTED}}"
    [[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "the version must look like 1.2.3"
    newer "${VERSION}" "${LAST}" || die "${VERSION} isn't newer than the last release, ${LAST}"
    TAG="v${VERSION}"
    if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
        die "${TAG} is already tagged"
    fi

    # 2. CHANGELOG.md: notes under [X.Y.Z], or under [Unreleased], which becomes [X.Y.Z].
    has_notes() { awk -v h="$1" 'index($0, h) == 1 {f = 1; next} f && /^## \[/ {exit} f && NF {n++} END {exit !n}' CHANGELOG.md; }
    if has_notes "## [${VERSION}]"; then
        DATE_UNRELEASED=false
    elif has_notes "## [Unreleased]"; then
        DATE_UNRELEASED=true
    else
        die "CHANGELOG.md has no notes under [${VERSION}] or [Unreleased]"
    fi

    # 3. Metainfo: a <release> for this version with notes, and the file validates.
    NOTES=$(xmllint --xpath "count(//release[@version='${VERSION}']/description)" "${METAINFO}")
    [ "${NOTES}" = 1 ] || die "${METAINFO} needs a <release version=\"${VERSION}\"> with a <description> of what's new"
    if ! appstreamcli validate --no-net "${METAINFO}" >/dev/null; then
        appstreamcli validate --no-net "${METAINFO}" || true
        die "${METAINFO} doesn't validate"
    fi

    # 4. The GNOME extension, if it changed since the last release: a new
    #    version-name, and a zip that passes EGO's checks.
    EXTENSION="unchanged since ${LAST_TAG}"
    if ! git diff --quiet "${LAST_TAG}" HEAD -- gnome-extension; then
        version_name() { node -p "JSON.parse(require('fs').readFileSync(0, 'utf8'))['version-name']"; }
        OLD_NAME=$(git show "${LAST_TAG}:gnome-extension/metadata.json" | version_name)
        NEW_NAME=$(version_name < gnome-extension/metadata.json)
        [ "${OLD_NAME}" != "${NEW_NAME}" ] || die "gnome-extension changed since ${LAST_TAG}, but its version-name is still ${NEW_NAME}"
        {{just_executable()}} extension-zip
        EXTENSION="${OLD_NAME} -> ${NEW_NAME}, zip validated"
    fi

    # 5. Tests, and the frontend builds.
    echo "==> Running the tests..."
    (cd src-tauri && cargo test --quiet)
    npm run build >/dev/null

    # From here on files change; a failure, or answering no, puts them back.
    RESTORE=true
    restore() {
        if [ "${RESTORE}" = true ]; then
            git checkout -q -- .
            echo "Files restored."
        fi
    }
    trap restore EXIT

    # 6. Set the version everywhere, date the changelog and the metainfo
    #    release, and point the Flatpak manifest at the new tag.
    TODAY=$(date +%Y-%m-%d)
    if [ "${CURRENT}" != "${VERSION}" ]; then
        {{just_executable()}} bump-version "${VERSION}" >/dev/null
    fi
    if [ "${DATE_UNRELEASED}" = true ]; then
        sed -i "s/^## \[Unreleased\]/## [${VERSION}] - ${TODAY}/" CHANGELOG.md
    fi
    sed -i -E "s|(<release version=\"${VERSION//./\\.}\" date=\")[^\"]*|\1${TODAY}|" "${METAINFO}"
    sed -i "s/tag: v.*/tag: ${TAG}/" "${MANIFEST}"

    # 7. Confirm.
    echo
    echo "=== GitHub release notes ==="
    scripts/release-notes.sh "${VERSION}"
    echo "=== Metainfo notes ==="
    xmllint --xpath "//release[@version='${VERSION}']/description" "${METAINFO}"
    echo
    echo "=== GNOME extension: ${EXTENSION} ==="
    echo
    git --no-pager diff --stat
    read -rp "Release ${TAG}? [y/N] " ANSWER
    [ "${ANSWER}" = y ] || die "cancelled"

    # 8. Commit, tag and push; the tag starts the release workflow.
    # Nothing to commit when releasing a version again after deleting its tag.
    git add -A
    git diff --cached --quiet || git commit -q -m "${TAG}"
    git tag "${TAG}"
    RESTORE=false
    git push -q --atomic origin main "${TAG}" \
        || die "the push failed. Once fixed: git push --atomic origin main ${TAG} && just release-finish ${VERSION}"
    echo "==> Pushed ${TAG}."
    {{just_executable()}} release-finish "${VERSION}" "{{output_dir}}"

# `just release` runs this; run it again after rerunning a failed workflow. The EGO
# zip is only saved when the GNOME extension changed since the previous release.
# Finish a pushed release: wait for GitHub, publish to the AUR, save the FriendlyHub files and EGO zip
release-finish version output_dir="~/Downloads":
    #!/usr/bin/env bash
    set -euo pipefail
    die() { echo "ERROR: $*" >&2; exit 1; }
    REPO=keithvassallomt/ClusterCut
    VERSION="{{version}}"
    TAG="v${VERSION}"
    OUTPUT_DIR="{{output_dir}}"
    OUTPUT_DIR="${OUTPUT_DIR/#\~/$HOME}"
    mkdir -p "${OUTPUT_DIR}"

    git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null || die "${TAG} isn't tagged"
    # aur-publish packages the version in package.json.
    [ "$(node -p "require('./package.json').version")" = "${VERSION}" ] || die "package.json isn't at ${VERSION}; check out ${TAG}"

    # 1. Wait for the release workflow.
    RUN=""
    for _ in $(seq 1 30); do
        RUN=$(gh run list -R "${REPO}" --workflow release.yml --branch "${TAG}" --event push --limit 1 --json databaseId --jq '.[0].databaseId // empty')
        [ -n "${RUN}" ] && break
        sleep 5
    done
    [ -n "${RUN}" ] || die "the release workflow didn't start; check: gh run list -R ${REPO} --workflow release.yml"
    echo "==> Waiting for the release workflow (the Flatpak and macOS builds take a while):"
    echo "    https://github.com/${REPO}/actions/runs/${RUN}"
    if ! gh run watch "${RUN}" -R "${REPO}" --exit-status --interval 30 >/dev/null; then
        FAILED=$(gh run view "${RUN}" -R "${REPO}" --json jobs --jq '[.jobs[] | select(.conclusion == "failure") | .name] | join(", ")')
        echo "ERROR: the release workflow failed in: ${FAILED}" >&2
        echo "  Logs:  gh run view ${RUN} -R ${REPO} --log-failed" >&2
        echo "  Retry: gh run rerun ${RUN} -R ${REPO} --failed, then: just release-finish ${VERSION}" >&2
        exit 1
    fi
    URL=$(gh release view "${TAG}" -R "${REPO}" --json url --jq .url) || die "the workflow passed but there's no GitHub release for ${TAG}"

    # 2. The FriendlyHub submission, as the workflow generated and built it.
    FRIENDLYHUB="${OUTPUT_DIR}/ClusterCut-${VERSION}-friendlyhub"
    rm -rf "${FRIENDLYHUB}"
    gh run download "${RUN}" -R "${REPO}" -n friendlyhub -D "${FRIENDLYHUB}"

    # 3. The EGO zip, if the extension changed since the release before this one.
    PREVIOUS=$(git tag --list 'v[0-9]*' --sort=-v:refname | awk -v t="${TAG}" 'found {print; exit} $0 == t {found = 1}')
    EGO_ZIP=""
    if ! git diff --quiet "${PREVIOUS}" "${TAG}" -- gnome-extension; then
        git diff --quiet "${TAG}" -- gnome-extension || die "gnome-extension has changed since ${TAG}; check out ${TAG} to build its zip"
        {{just_executable()}} extension-zip
        EGO_ZIP="${OUTPUT_DIR}/clustercut@keithvassallo.com.shell-extension.zip"
        cp clustercut@keithvassallo.com.shell-extension.zip "${EGO_ZIP}"
    fi

    # 4. The AUR. A failure here leaves the rest of the release standing.
    echo "==> Publishing to the AUR..."
    AUR_OK=true
    {{just_executable()}} aur-publish || AUR_OK=false

    echo
    echo "============================================"
    echo " ClusterCut ${VERSION} is released"
    echo "============================================"
    echo "GitHub:      ${URL}"
    if [ "${AUR_OK}" = true ]; then
        echo "AUR:         https://aur.archlinux.org/packages/clustercut-bin"
    else
        echo "AUR:         FAILED (see above). Once fixed: just aur-publish"
    fi
    echo "FriendlyHub: ${FRIENDLYHUB}/ (the four files for the update PR)"
    if [ -n "${EGO_ZIP}" ]; then
        echo "EGO:         ${EGO_ZIP} (upload at https://extensions.gnome.org/upload/)"
    else
        echo "EGO:         nothing to upload, the extension is unchanged since ${PREVIOUS}"
    fi
    [ "${AUR_OK}" = true ]

# Run the release workflow on GitHub without publishing anything (it must be on main first)
release-dry-run ref="main":
    gh workflow run release.yml -R keithvassallomt/ClusterCut --ref {{ref}}
    @echo "Started: https://github.com/keithvassallomt/ClusterCut/actions/workflows/release.yml"

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
    # we don't reinstall shexli on every zip build, and rebuild it when it stops
    # working (a Python upgrade breaks it). tree-sitter 0.26 crashes shexli 0.2.1.
    install_shexli() { .venv-shexli/bin/pip install -q -U shexli 'tree-sitter<0.26'; }
    if [ ! -d .venv-shexli ] || ! install_shexli 2>/dev/null; then
        echo "==> Creating shexli virtualenv..."
        rm -rf .venv-shexli
        python3 -m venv .venv-shexli
        install_shexli
    fi

    echo "==> Validating with shexli..."
    ZIP=clustercut@keithvassallo.com.shell-extension.zip
    REPORT=$(mktemp)
    trap 'rm -f "${REPORT}"' EXIT
    if ! .venv-shexli/bin/shexli --format json "${ZIP}" > "${REPORT}"; then
        rm -f "${ZIP}"
        echo "ERROR: shexli didn't run; zip removed."
        exit 1
    fi
    .venv-shexli/bin/shexli "${ZIP}"
    # shexli exits 0 whatever it finds, so the verdict comes from its report: any
    # error fails the zip, except the false positives allowed here. Warnings and
    # manual-review findings are for EGO's reviewers and don't.
    #   EGO-M-004: shexli 0.2.1 takes any shell-version above 50 for a future
    #   release, but GNOME 51 is out, and EGO accepted it for extension 4.1.
    ERRORS=$(python3 -c 'import json, sys; findings = json.load(open(sys.argv[1]))["findings"]; print(" ".join(sorted({f["rule_id"] for f in findings if f["severity"] == "error"} - set(sys.argv[2:]))))' "${REPORT}" EGO-M-004)
    if [ -n "${ERRORS}" ]; then
        rm -f "${ZIP}"
        echo "ERROR: shexli found errors (${ERRORS}); zip removed."
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

    # Resolve the signing identity HERE, in the project tree, where any
    # per-directory includeIf still applies. The AUR checkout below lives in a temp
    # dir that only sees global config, so whatever it would resolve there may not
    # be the key you actually sign with (or may not resolve at all).
    SIGN_FORMAT=$(git config --get gpg.format || echo "")
    SIGN_KEY=$(git config --get user.signingkey || echo "")
    SIGN_ON=$(git config --get commit.gpgsign || echo "false")

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
    # Stage first, then ask porcelain. On the very first push the AUR repo is empty:
    # both files are untracked and there is no HEAD to diff against, so the plain
    # `git diff`/`git diff --cached` pair reports "no changes" and skips the push.
    git add PKGBUILD .SRCINFO
    if [ -z "$(git status --porcelain)" ]; then
        echo "==> AUR already up to date at ${VERSION}; nothing to push."
        exit 0
    fi
    # Sign with the same key the project signs with, carried in from above. Without
    # this the commit inherits global config, where a signingkey that doesn't
    # resolve outside the project tree aborts the commit outright.
    if [ "${SIGN_ON}" = "true" ] && [ -n "${SIGN_KEY}" ]; then
        git -c commit.gpgsign=true \
            -c gpg.format="${SIGN_FORMAT}" \
            -c user.signingkey="${SIGN_KEY}" \
            commit --quiet -m "clustercut-bin ${VERSION}"
    else
        git commit --quiet -m "clustercut-bin ${VERSION}"
    fi
    # The AUR's branch is master. A clone of an *empty* repo takes its branch name
    # from init.defaultBranch locally, which may well be main -- so name the remote
    # ref explicitly rather than relying on the local branch being called master.
    git push --quiet origin HEAD:refs/heads/master
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
        echo "       The release workflow attaches it; check that ${TAG}'s run finished."
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
