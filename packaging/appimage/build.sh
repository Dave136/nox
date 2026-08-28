#!/usr/bin/env bash
# Builds target/release/nox and packages it as an AppImage.
# Requires network access on first run to fetch linuxdeploy (cached after that).
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
tools_dir="$script_dir/tools"
appdir="$script_dir/AppDir"

mkdir -p "$tools_dir"

fetch_tool() {
  local name="$1" url="$2" path="$tools_dir/$1"
  if [[ ! -x "$path" ]]; then
    echo "Fetching $name..."
    curl -fL -o "$path" "$url"
    chmod +x "$path"
  fi
}

fetch_tool linuxdeploy \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage"
fetch_tool linuxdeploy-plugin-appimage \
  "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-x86_64.AppImage"

echo "Building release binary..."
cargo build --locked --release --bin nox --manifest-path "$repo_root/Cargo.toml"

rm -rf "$appdir"
mkdir -p "$appdir/usr/bin"
install -Dm755 "$repo_root/target/release/nox" "$appdir/usr/bin/nox"

cd "$script_dir"
PATH="$tools_dir:$PATH" "$tools_dir/linuxdeploy" \
  --appdir "$appdir" \
  --executable "$appdir/usr/bin/nox" \
  --desktop-file "$repo_root/packaging/nox.desktop" \
  --icon-file "$repo_root/packaging/icons/hicolor/scalable/apps/nox.svg" \
  --output appimage

echo "Done: $(ls "$script_dir"/Nox-*.AppImage 2>/dev/null | tail -1)"
