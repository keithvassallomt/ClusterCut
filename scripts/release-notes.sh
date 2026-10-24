#!/usr/bin/env bash
# Print the GitHub release notes for a version: packaging/release-notes-template.md
# with @VERSION@ substituted and @CHANGELOG@ replaced by that version's
# CHANGELOG.md section, so every release keeps the same shape (install
# instructions, tips, Full Changelog footer). Fails if the section is missing
# or empty. Used by the release workflow and `just release`'s preview.
set -euo pipefail

VERSION="${1:?usage: scripts/release-notes.sh X.Y.Z}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

SECTION=$(awk -v ver="${VERSION}" '
    index($0, "## [" ver "]") == 1 { inside = 1; next }
    inside && /^## \[/ { exit }
    inside {
        # Drop the blank line that follows the version heading; the template
        # already spaces the changelog away from the title.
        if (!started && $0 ~ /^[[:space:]]*$/) next
        started = 1
        print
    }
' "${ROOT}/CHANGELOG.md")

if [ -z "${SECTION//[[:space:]]/}" ]; then
    echo "ERROR: CHANGELOG.md has no section for ${VERSION}." >&2
    exit 1
fi

# Through the environment, not -v: awk would interpret backslashes in -v values.
SECTION="${SECTION}" awk -v ver="${VERSION}" '
    /@CHANGELOG@/ { print ENVIRON["SECTION"]; print ""; next }
    { gsub(/@VERSION@/, ver); print }
' "${ROOT}/packaging/release-notes-template.md"
