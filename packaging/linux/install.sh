#!/bin/sh
# Installs the release build, its desktop entry and icon for the current user.
# Run after `cargo build --release -p viewer-app`; set CARGO_TARGET_DIR if the build used another target directory.
set -eu

repo=$(cd "$(dirname "$0")/../.." && pwd)
target=${CARGO_TARGET_DIR:-$repo/target}
app_id=io.github.greenwojakk.TPUI
bin=$HOME/.local/bin/tpui-viewer

install -Dm755 "$target/release/viewer-app" "$bin"
install -Dm644 "$repo/crates/viewer-app/assets/logo.png" \
    "$HOME/.local/share/icons/hicolor/512x512/apps/$app_id.png"
mkdir -p "$HOME/.local/share/applications"
sed "s|@EXEC@|$bin|" "$repo/packaging/linux/$app_id.desktop" \
    > "$HOME/.local/share/applications/$app_id.desktop"

echo "Installed $bin and the $app_id desktop entry."
