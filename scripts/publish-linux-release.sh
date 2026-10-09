#!/usr/bin/env bash
# Publish the AirFlash Linux preview release once a GitHub token is available.
#
#   GH_TOKEN=<token> bash scripts/publish-linux-release.sh
#
# The artifacts are the two x86_64 AppImages in artifacts/release plus their
# SHA256SUMS.txt. The tag is created and pushed here if it does not exist yet;
# everything after that is a single `gh release create` call.
set -euo pipefail

REPO=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=${VERSION:-0.4.0}
TAG="v$VERSION"
ARTIFACTS=(
    "$REPO/artifacts/release/AirFlash-$VERSION-x86_64.AppImage"
    "$REPO/artifacts/release/AirFlash-$VERSION-x86_64-full.AppImage"
    "$REPO/artifacts/release/SHA256SUMS.txt"
)

command -v gh >/dev/null 2>&1 || { echo "gh is required: https://cli.github.com" >&2; exit 1; }
[ -n "${GH_TOKEN:-}" ] || { echo "Set GH_TOKEN with a token that can create releases." >&2; exit 1; }
for artifact in "${ARTIFACTS[@]}"; do
    [ -s "$artifact" ] || { echo "missing artifact: $artifact" >&2; exit 1; }
done

cd "$REPO"
if ! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    git tag -a "$TAG" -m "AirFlash $VERSION · Linux preview"
    git push git@github.com:HaochenH/AirFlash.git "refs/tags/$TAG"
fi

gh release create "$TAG" "${ARTIFACTS[@]}" \
    --repo HaochenH/AirFlash \
    --title "AirFlash $VERSION · Linux preview" \
    --notes-file docs/LINUX-RELEASE-NOTES.md \
    --latest

echo "published $TAG"
