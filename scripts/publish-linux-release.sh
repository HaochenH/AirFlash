#!/usr/bin/env bash
# Publish the AirFlash Linux preview release once GitHub auth is available.
#
#   gh auth login
#   bash scripts/publish-linux-release.sh
#
# GH_TOKEN is also honoured when set, which is what CI should use; otherwise the
# script falls back to credentials stored by `gh auth login`.
#
# The artifacts are the two x86_64 AppImages in artifacts/release plus their
# SHA256SUMS.txt. The tag is created and pushed here if it does not exist yet;
# everything after that is a single `gh release create` call.
set -euo pipefail

REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=${VERSION:-0.4.0}
TAG="v$VERSION"
# Push the tag through the checked-out remote so we use whichever identity the
# clone is configured with, instead of a hardcoded SSH URL.
REMOTE=${REMOTE:-origin}
ARTIFACTS=(
    "$REPO/artifacts/release/AirFlash-$VERSION-x86_64.AppImage"
    "$REPO/artifacts/release/AirFlash-$VERSION-x86_64-full.AppImage"
    "$REPO/artifacts/release/SHA256SUMS.txt"
)

command -v gh >/dev/null 2>&1 || { echo "gh is required: https://cli.github.com" >&2; exit 1; }

# `gh auth login` stores credentials in gh's own config instead of the
# environment, so fall back to it when GH_TOKEN is unset. This lets a local
# run work straight after login and keeps CI usable with a plain token.
if [ -z "${GH_TOKEN:-}" ]; then
    GH_TOKEN=$(gh auth token 2>/dev/null || true)
fi
[ -n "$GH_TOKEN" ] || {
    echo "No GitHub token: set GH_TOKEN, or run 'gh auth login' first." >&2
    exit 1
}
export GH_TOKEN

for artifact in "${ARTIFACTS[@]}"; do
    [ -s "$artifact" ] || { echo "missing artifact: $artifact" >&2; exit 1; }
done

cd "$REPO"
if ! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    git tag -a "$TAG" -m "AirFlash $VERSION · Linux preview"
    git push "$REMOTE" "refs/tags/$TAG"
fi

gh release create "$TAG" "${ARTIFACTS[@]}" \
    --repo HaochenH/AirFlash \
    --title "AirFlash $VERSION · Linux preview" \
    --notes-file docs/LINUX-RELEASE-NOTES.md \
    --latest

echo "published $TAG"
