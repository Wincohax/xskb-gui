#!/usr/bin/env bash
# Build a self-contained x86_64 AppImage for XSKB (XsEngine keyboard controller).
#
# Run it where the GPUI build dependencies are installed (on immutable systems
# such as Bazzite, that means inside the Fedora distrobox). It can be run from
# any directory. Output: target/appimage/XSKB-x86_64.AppImage
#
# The AppImage does not bundle pw-record: the audio-reactive mode uses the
# PipeWire tools already installed on the host.
#
# Optional environment variables:
#   LINUXDEPLOY       path to the linuxdeploy AppImage (downloaded if missing)
#   APPIMAGETOOL      path to the appimagetool AppImage (downloaded if missing)
#   APPIMAGE_TOOLS_DIR  where downloaded tools live (default: ~/.cache/appimage-tools)
set -euo pipefail

APP_ID="io.github.wincohax.Xskb"
BIN_NAME="xskb-gui"
CARGO_ARGS=()
OUTPUT_NAME="XSKB-x86_64.AppImage"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

die() { echo "error: $*" >&2; exit 1; }
step() { echo; echo "==> $*"; }

# Cargo may live in ~/.cargo/bin when the shell did not source the rustup env.
if ! command -v cargo >/dev/null 2>&1 && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || die "cargo not found; install Rust or run this inside the build distrobox"

TOOLS_DIR="${APPIMAGE_TOOLS_DIR:-$HOME/.cache/appimage-tools}"
LINUXDEPLOY="${LINUXDEPLOY:-$TOOLS_DIR/linuxdeploy-x86_64.AppImage}"
APPIMAGETOOL="${APPIMAGETOOL:-$TOOLS_DIR/appimagetool-x86_64.AppImage}"
LINUXDEPLOY_URL="https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"

# The tools are AppImages too. Extract-and-run works without FUSE (containers, CI).
export APPIMAGE_EXTRACT_AND_RUN=1

fetch_tool() {
    local path="$1" url="$2"
    [[ -x "$path" ]] && return 0
    command -v curl >/dev/null 2>&1 || die "curl is needed to download $(basename "$path")"
    step "Downloading $(basename "$path") to $path"
    mkdir -p "$(dirname "$path")"
    curl -fL --retry 3 -o "$path.part" "$url"
    chmod +x "$path.part"
    mv "$path.part" "$path"
}

fetch_tool "$LINUXDEPLOY" "$LINUXDEPLOY_URL"
fetch_tool "$APPIMAGETOOL" "$APPIMAGETOOL_URL"

step "Building $BIN_NAME (release)"
cargo build --release "${CARGO_ARGS[@]}"

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
BIN="$TARGET_DIR/release/$BIN_NAME"
[[ -x "$BIN" ]] || die "release binary not found at $BIN"

DESKTOP_SRC="packaging/$APP_ID.desktop"
ICON_256="packaging/icons/256x256/$APP_ID.png"
[[ -f "$DESKTOP_SRC" ]] || die "missing $DESKTOP_SRC"
[[ -f "$ICON_256" ]] || die "missing $ICON_256"

OUT_DIR="$REPO_ROOT/target/appimage"
APPDIR="$OUT_DIR/AppDir"

step "Creating AppDir at $APPDIR"
rm -rf "$APPDIR"
install -d "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/scalable/apps"
install -m 755 "$BIN" "$APPDIR/usr/bin/$BIN_NAME"
install -m 644 "$DESKTOP_SRC" "$APPDIR/usr/share/applications/$APP_ID.desktop"
install -m 644 "packaging/icons/$APP_ID.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
for dir in packaging/icons/*x*/; do
    size="$(basename "$dir")"
    [[ -f "$dir/$APP_ID.png" ]] || continue
    install -d "$APPDIR/usr/share/icons/hicolor/$size/apps"
    install -m 644 "$dir/$APP_ID.png" "$APPDIR/usr/share/icons/hicolor/$size/apps/$APP_ID.png"
done
# The AppDir root needs the desktop file, the icon and .DirIcon.
install -m 644 "$DESKTOP_SRC" "$APPDIR/$APP_ID.desktop"
install -m 644 "$ICON_256" "$APPDIR/$APP_ID.png"
install -m 644 "$ICON_256" "$APPDIR/.DirIcon"

step "Bundling shared libraries with linuxdeploy"
# linuxdeploy applies the standard AppImage excludelist. The patterns below
# also keep GPU, display-server and C-runtime libraries on the host, where
# they must come from.
"$LINUXDEPLOY" --appdir "$APPDIR" \
    --executable "$APPDIR/usr/bin/$BIN_NAME" \
    --desktop-file "$DESKTOP_SRC" \
    --icon-file "$ICON_256" \
    --exclude-library 'libGL.so*' --exclude-library 'libGLX*' --exclude-library 'libGLdispatch*' \
    --exclude-library 'libEGL*' --exclude-library 'libvulkan*' \
    --exclude-library 'libwayland-*' --exclude-library 'libxcb*' --exclude-library 'libX11*' \
    --exclude-library 'libXau*' --exclude-library 'libXdmcp*' \
    --exclude-library 'libfontconfig*' --exclude-library 'libfreetype*' \
    --exclude-library 'libc.so*' --exclude-library 'libm.so*' --exclude-library 'libdl.so*' \
    --exclude-library 'libpthread.so*' --exclude-library 'librt.so*' --exclude-library 'ld-linux*'

# Keep the desktop file at the root even if linuxdeploy rewrote its own copy.
install -m 644 "$DESKTOP_SRC" "$APPDIR/$APP_ID.desktop"

step "Writing AppRun"
# linuxdeploy may have left a symlink here; remove it first so we never write through it.
rm -f "$APPDIR/AppRun"
cat > "$APPDIR/AppRun" <<EOF
#!/bin/sh
# Launch the bundled binary. Its RPATH already points at usr/lib, so
# LD_LIBRARY_PATH is left alone (it would leak into host helpers such as pw-record).
HERE="\$(dirname "\$(readlink -f "\$0")")"
exec "\$HERE/usr/bin/$BIN_NAME" "\$@"
EOF
chmod 755 "$APPDIR/AppRun"

step "Creating AppImage with appimagetool"
OUTPUT="$OUT_DIR/$OUTPUT_NAME"
rm -f "$OUTPUT"
ARCH=x86_64 "$APPIMAGETOOL" --no-appstream "$APPDIR" "$OUTPUT"

echo
echo "Built $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"
