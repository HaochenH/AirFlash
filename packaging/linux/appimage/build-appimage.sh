#!/usr/bin/env bash
# Repeatable AppImage builds for the Linux slice.
#
# VARIANT=backend  headless only:  AirFlash-<version>-x86_64.AppImage
# VARIANT=full     backend + GUI: AirFlash-<version>-x86_64-full.AppImage
#
# Both write to dist/AppImage with a SHA256SUMS.txt next to the artifacts.
#
# The script is deterministic and safe to re-run: it rebuilds the AppDir from
# scratch, resolves the Rust toolchain for the selected target, and never
# touches the Windows release outputs (dist/AirFlash.exe, dist/AirFlash-*.msi).
#
# Environment overrides:
#   VARIANT                backend (default) or full
#   VERSION                version embedded in names and the desktop entry
#   TARGET                 rust target triple (default x86_64-unknown-linux-gnu)
#   ARCH                   AppImage arch tag (default x86_64)
#   DOTNET_RUNTIME         dotnet runtime identifier for the GUI (default linux-x64)
#   OUTPUT_DIR             artifact directory (default dist/AppImage)
#   BUILD_DIR              scratch directory (default build/appimage)
#   AIRFLASH_APPIMAGETOOL  path to an existing appimagetool binary
#   SKIP_APPIMAGETOOL_DOWNLOAD=1  fail instead of downloading the tool
set -euo pipefail

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
VERSION=${VERSION:-}
VARIANT=${VARIANT:-backend}
case "$VARIANT" in
    backend) SUFFIX="" ;;
    full) SUFFIX="-full" ;;
    *) fail "VARIANT must be backend or full (got '$VARIANT')" ;;
esac
TARGET=${TARGET:-x86_64-unknown-linux-gnu}
ARCH=${ARCH:-x86_64}
DOTNET_RUNTIME=${DOTNET_RUNTIME:-linux-x64}
OUTPUT_DIR=${OUTPUT_DIR:-$REPO_ROOT/dist/AppImage}
BUILD_DIR=${BUILD_DIR:-$REPO_ROOT/build/appimage}
APP_NAME=AirFlash
BIN_NAME=airflash-cli
DESKTOP_ID=airflash-cli
ICON_NAME=airflash-cli
GUI_NAME=airflash-ui

log() { printf '==> %s\n' "$*"; }
fail() { printf 'appimage build failed: %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null 2>&1 || fail "cargo is not on PATH; install the Rust toolchain"
command -v curl >/dev/null 2>&1 || fail "curl is required to fetch appimagetool"
if [ "$VARIANT" = "full" ]; then
    command -v dotnet >/dev/null 2>&1 || fail "VARIANT=full needs the .NET SDK on PATH"
fi

resolve_version() {
    if [ -n "$VERSION" ]; then
        printf '%s' "$VERSION"
        return
    fi
    if command -v git >/dev/null 2>&1 && git -C "$REPO_ROOT" describe --tags --abbrev=0 >/dev/null 2>&1; then
        git -C "$REPO_ROOT" describe --tags --abbrev=0 | sed 's/^v//'
        return
    fi
    # Cargo.toml is the last word; it always carries the crate version.
    sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/native/airflash-engine/Cargo.toml" | head -1
}
VERSION=${VERSION:-$(resolve_version)}
[ -n "$VERSION" ] || fail "cannot determine a version; set VERSION="
log "variant $VARIANT, version $VERSION, target $TARGET, arch $ARCH"

# The CLI is a feature-gated binary; this is the only place it is enabled.
if command -v rustup >/dev/null 2>&1; then
    if ! rustup target list --installed | grep -qx "$TARGET"; then
        log "installing rust target $TARGET"
        rustup target add "$TARGET" || fail "cannot install target $TARGET"
    fi
fi

# Cross builds need a linker for the target; honour an explicit override first.
HOST_TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
if [ "$HOST_TRIPLE" != "$TARGET" ] && [ -z "${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER:-}" ]; then
    # Debian names the cross compiler x86_64-linux-gnu-gcc, not <triple>-gcc.
    CROSS_LINKER=$(printf '%s' "$TARGET" | sed 's/-unknown-/-/' )-gcc
    if command -v "$CROSS_LINKER" >/dev/null 2>&1; then
        log "cross-linking $TARGET with $CROSS_LINKER"
        export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=$CROSS_LINKER
    else
        fail "cross build to $TARGET needs $CROSS_LINKER or CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER"
    fi
fi

BUILD_PROFILE=release
log "building $BIN_NAME ($TARGET, $BUILD_PROFILE)"
cargo build --manifest-path "$REPO_ROOT/native/airflash-engine/Cargo.toml" \
    --release --locked --target "$TARGET" --features cli --bin "$BIN_NAME" \
    || fail "cargo build failed"
cargo build --manifest-path "$REPO_ROOT/native/airflash-engine/Cargo.toml" \
    --release --locked --target "$TARGET" --bin airflash-engine \
    || fail "cargo build failed"

case "$TARGET" in
    *windows*|*apple*) fail "the AppImage slice is Linux-only (got $TARGET)" ;;
esac
if [ "$VARIANT" = "full" ]; then
    log "publishing the Avalonia UI (self-contained, $DOTNET_RUNTIME)"
    GUI_PUBLISH="$BUILD_DIR/gui"
    rm -rf "$GUI_PUBLISH"
    dotnet publish "$REPO_ROOT/desktop/AirFlash.UI/AirFlash.UI.csproj" \
        -c Release -r "$DOTNET_RUNTIME" --self-contained true \
        -p:PublishSingleFile=false -p:DeterministicBuild=true \
        -o "$GUI_PUBLISH" >/dev/null || fail "dotnet publish failed"
    [ -x "$GUI_PUBLISH/AirFlash.UI" ] || fail "the published UI is missing"
fi

BUILD_ROOT="$REPO_ROOT/native/airflash-engine/target/$TARGET/$BUILD_PROFILE"
CLI_BINARY="$BUILD_ROOT/$BIN_NAME"
ENGINE_BINARY="$BUILD_ROOT/airflash-engine"
[ -x "$CLI_BINARY" ] || fail "$CLI_BINARY missing after build"
[ -x "$ENGINE_BINARY" ] || fail "$ENGINE_BINARY missing after build"

APPDIR="$BUILD_DIR/$APP_NAME.AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/512x512/apps" \
    "$APPDIR/usr/share/licenses/$DESKTOP_ID" "$APPDIR/usr/share/doc/$DESKTOP_ID"

install -m 0755 "$CLI_BINARY" "$APPDIR/usr/bin/$BIN_NAME"
install -m 0755 "$ENGINE_BINARY" "$APPDIR/usr/bin/airflash-engine"
install -m 0755 "$REPO_ROOT/packaging/linux/appimage/AppRun" "$APPDIR/AppRun"
GUI_LAUNCHER="$APPDIR/usr/bin/airflash-ui"
if [ "$VARIANT" = "full" ]; then
    # The Avalonia app and its runtime live in usr/lib; a small wrapper starts it
    # so PATH and the runtime dir are already set up by AppRun.
    mkdir -p "$APPDIR/usr/lib/airflash-ui"
    cp -a "$GUI_PUBLISH/." "$APPDIR/usr/lib/airflash-ui/"
    cat > "$GUI_LAUNCHER" <<'LAUNCHER'
#!/bin/sh
set -eu
HERE=$(CDPATH= cd -- "$(dirname -- "$0")/../lib/airflash-ui" && pwd)
exec "$HERE/AirFlash.UI" "$@"
LAUNCHER
    chmod 0755 "$GUI_LAUNCHER"
fi
install -m 0644 "$REPO_ROOT/desktop/AirFlash.App/Assets/app.png" \
    "$APPDIR/usr/share/icons/hicolor/512x512/apps/$ICON_NAME.png"
# appimagetool resolves the Icon key from the AppDir root; .DirIcon follows.
cp "$APPDIR/usr/share/icons/hicolor/512x512/apps/$ICON_NAME.png" "$APPDIR/$ICON_NAME.png"
cp "$APPDIR/usr/share/icons/hicolor/512x512/apps/$ICON_NAME.png" "$APPDIR/.DirIcon"

# Version naming: the desktop entry, a plain VERSION file and the artifact name.
# appimagetool reads the desktop entry from the AppDir root; the copy under
# usr/share/applications keeps desktop integration when the AppDir is extracted.
sed -e "s|@VERSION@|$VERSION|g" \
    "$REPO_ROOT/packaging/linux/appimage/$DESKTOP_ID.desktop" \
    > "$APPDIR/$DESKTOP_ID.desktop"
chmod 0644 "$APPDIR/$DESKTOP_ID.desktop"
cp "$APPDIR/$DESKTOP_ID.desktop" "$APPDIR/usr/share/applications/$DESKTOP_ID.desktop"
if [ "$VARIANT" = "full" ]; then
    # A second entry launches the GUI; the CLI entry stays Terminal=true.
    sed -e "s|@VERSION@|$VERSION|g" \
        -e "s|^Exec=.*|Exec=airflash-ui|" \
        -e "s|^Terminal=.*|Terminal=false|" \
        -e "s|^Comment=.*|Comment=AirFlash sender with its graphical panel|" \
        "$REPO_ROOT/packaging/linux/appimage/$DESKTOP_ID.desktop" \
        > "$APPDIR/$GUI_NAME.desktop"
    chmod 0644 "$APPDIR/$GUI_NAME.desktop"
    cp "$APPDIR/$GUI_NAME.desktop" "$APPDIR/usr/share/applications/$GUI_NAME.desktop"
fi
printf 'AirFlash %s\nAirPlay 2 sender, headless Linux slice.\n' "$VERSION" \
    > "$APPDIR/usr/share/doc/$DESKTOP_ID/README"
printf '%s\n' "$VERSION" > "$APPDIR/VERSION"
install -m 0644 "$REPO_ROOT/LICENSE-GPLv3" "$APPDIR/usr/share/licenses/$DESKTOP_ID/LICENSE-GPLv3"
install -m 0644 "$REPO_ROOT/LICENSE-COMMERCIAL.md" \
    "$APPDIR/usr/share/licenses/$DESKTOP_ID/LICENSE-COMMERCIAL.md"
log "AppDir assembled at $APPDIR"

# Sanity checks that do not need appimagetool.
[ -x "$APPDIR/AppRun" ] || fail "AppRun must be executable"
[ -s "$APPDIR/VERSION" ] || fail "AppDir/VERSION is missing"
grep -q "X-AppImage-Version=$VERSION" "$APPDIR/usr/share/applications/$DESKTOP_ID.desktop" \
    || fail "desktop entry is missing the version"
for key in Name Exec Icon Categories Type; do
    grep -q "^$key=" "$APPDIR/usr/share/applications/$DESKTOP_ID.desktop" \
        || fail "desktop entry is missing $key"
done
grep -q "usr/bin/$BIN_NAME" "$APPDIR/AppRun" || fail "AppRun does not launch $BIN_NAME"
[ -s "$APPDIR/.DirIcon" ] || fail "AppDir icon (.DirIcon) is missing"
[ -s "$APPDIR/$ICON_NAME.png" ] || fail "AppDir icon ($ICON_NAME.png) is missing"
if [ "$VARIANT" = "full" ]; then
    [ -x "$GUI_LAUNCHER" ] || fail "the GUI launcher is missing or not executable"
    [ -x "$APPDIR/usr/lib/airflash-ui/AirFlash.UI" ] || fail "the published UI executable is missing"
    [ -f "$APPDIR/$GUI_NAME.desktop" ] || fail "the GUI desktop entry is missing"
    grep -q "^Exec=airflash-ui$" "$APPDIR/$GUI_NAME.desktop" || fail "the GUI desktop entry must launch the UI"
    grep -q "^Terminal=false$" "$APPDIR/$GUI_NAME.desktop" || fail "the GUI entry must not claim a terminal"
fi
[ -x "$APPDIR/usr/bin/$BIN_NAME" ] || fail "$BIN_NAME is not executable"
if [ "$HOST_TRIPLE" = "$TARGET" ]; then
    "$APPDIR/AppRun" version >/dev/null || fail "AppRun cannot execute the CLI"
else
    log "skipping AppRun execution check: $TARGET binaries cannot run on $HOST_TRIPLE"
fi

# appimagetool itself is an AppImage, which needs FUSE. GitHub runners and many
# containers have none, so extract and run the payload instead of executing it.
TOOL=${AIRFLASH_APPIMAGETOOL:-}
if [ -z "$TOOL" ]; then
    TOOLS_DIR="$BUILD_DIR/tools"
    mkdir -p "$TOOLS_DIR"
    TOOL="$TOOLS_DIR/appimagetool"
    if [ ! -x "$TOOL" ]; then
        if [ "${SKIP_APPIMAGETOOL_DOWNLOAD:-0}" = "1" ]; then
            fail "appimagetool unavailable and SKIP_APPIMAGETOOL_DOWNLOAD=1"
        fi
        log "downloading appimagetool (continuous build)"
        ARCHIVE="$TOOLS_DIR/appimagetool.AppImage"
        curl -fsSL --retry 3 -o "$ARCHIVE" \
            https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage \
            || fail "cannot download appimagetool"
        chmod +x "$ARCHIVE"
        rm -rf "$TOOLS_DIR/squashfs-root"
        # Extraction needs no FUSE, but the tool itself must match this machine.
        (cd "$TOOLS_DIR" && "$ARCHIVE" --appimage-extract >/dev/null) \
            || fail "cannot extract appimagetool (missing FUSE or wrong architecture); install appimagetool and set AIRFLASH_APPIMAGETOOL"
        rm -f "$ARCHIVE"
        printf '#!/bin/sh\nexec "%s/squashfs-root/AppRun" "$@"\n' "$TOOLS_DIR" > "$TOOL"
        chmod +x "$TOOL"
    fi
fi
[ -x "$TOOL" ] || fail "appimagetool at $TOOL is not executable"

mkdir -p "$OUTPUT_DIR"
ARTIFACT="$OUTPUT_DIR/$APP_NAME-$VERSION-$ARCH$SUFFIX.AppImage"
log "packaging $ARTIFACT"
rm -f "$ARTIFACT"
ARCH=$ARCH "$TOOL" --no-appstream "$APPDIR" "$ARTIFACT" >/dev/null \
    || fail "appimagetool failed"
[ -s "$ARTIFACT" ] || fail "AppImage was not produced"
SIZE=$(wc -c < "$ARTIFACT")
[ "$SIZE" -gt 1000000 ] || fail "AppImage is suspiciously small ($SIZE bytes)"

( cd "$OUTPUT_DIR" && sha256sum "$(basename "$ARTIFACT")" > SHA256SUMS.txt )
if [ "$VARIANT" = "full" ]; then
    log "full AppImage: no arguments launches the GUI; 'airflash-cli <command>' still works"
fi
log "built $ARTIFACT ($SIZE bytes)"
log "sha256 $(cut -d' ' -f1 < "$OUTPUT_DIR/SHA256SUMS.txt")"
printf '%s\n' "$ARTIFACT"
