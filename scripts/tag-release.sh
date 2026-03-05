#!/bin/bash
# Create and push a release or kernel tag.
#
# Usage:
#   ./scripts/tag-release.sh 0.1.0              # creates and pushes v0.1.0
#   ./scripts/tag-release.sh 0.1.0 --kernel     # creates and pushes kernel-v0.1.0
#   ./scripts/tag-release.sh 0.1.0 --dry-run    # show what would happen
#
# Options:
#   --kernel    Use kernel-v prefix instead of v
#   --dry-run   Print commands without executing
#   --force     Overwrite existing tag

set -euo pipefail

VERSION=""
PREFIX="v"
DRY_RUN=false
FORCE=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --kernel)  PREFIX="kernel-v"; shift ;;
        --dry-run) DRY_RUN=true; shift ;;
        --force)   FORCE=true; shift ;;
        --help|-h) sed -n '2,12p' "$0" | sed 's/^# \?//'; exit 0 ;;
        -*)        echo "Unknown option: $1" >&2; exit 1 ;;
        *)         VERSION="$1"; shift ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 <version> [--kernel] [--dry-run] [--force]" >&2
    exit 1
fi

TAG="${PREFIX}${VERSION}"

run() {
    if $DRY_RUN; then
        echo "[dry-run] $*"
    else
        echo "=> $*"
        "$@"
    fi
}

FORCE_FLAG=""
if $FORCE; then
    FORCE_FLAG="-f"
fi

echo "Tag: ${TAG}"
echo ""

run git tag $FORCE_FLAG "$TAG"
run git push $FORCE_FLAG origin "$TAG"

if ! $DRY_RUN; then
    echo ""
    echo "Done! Tag ${TAG} pushed to origin."
fi
