#!/usr/bin/env bash
set -euo pipefail

REPO="${1:-WynonnaSR/mpris-bridge}"
BIN_DIR="${HOME}/.local/bin"
UNIT_DIR="${HOME}/.config/systemd/user"

if ! command -v curl >/dev/null 2>&1; then
  echo "Error: curl is required." >&2
  exit 1
fi

arch=$(uname -m)
case "$arch" in
  x86_64|amd64) TARGET="x86_64-unknown-linux-gnu";;
  aarch64|arm64) TARGET="aarch64-unknown-linux-gnu";;
  *) echo "Unsupported arch: $arch"; exit 1;;
esac

api_url="https://api.github.com/repos/${REPO}/releases/latest"
json="$(curl -sSL "$api_url")"

tag="$(printf '%s\n' "$json" | sed -n 's/ *"tag_name": *"\(.*\)".*/\1/p' | head -n1)"
[ -n "$tag" ] || { echo "Failed to get latest tag from $api_url"; exit 1; }

asset_url="$(printf '%s\n' "$json" | sed -n "s# *\"browser_download_url\": *\"\(.*mpris-bridge-${TARGET}\.tar\.gz\)\"#\1#p" | head -n1)"
[ -n "$asset_url" ] || { echo "No asset found for ${TARGET} in ${REPO} ${tag}"; exit 1; }

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "Downloading ${asset_url}"
curl -sSL "$asset_url" -o "$tmpdir/mpris-bridge.tar.gz"
mkdir -p "$tmpdir/unpack"
tar -xzf "$tmpdir/mpris-bridge.tar.gz" -C "$tmpdir/unpack"

mkdir -p "$BIN_DIR" "$UNIT_DIR"
install -Dm755 "$tmpdir/unpack/mpris-bridged"  "${BIN_DIR}/mpris-bridged"
install -Dm755 "$tmpdir/unpack/mpris-bridgec" "${BIN_DIR}/mpris-bridgec"
install -Dm644 "packaging/systemd/mpris-bridged.service" "${UNIT_DIR}/mpris-bridged.service"

systemctl --user daemon-reload
systemctl --user enable --now mpris-bridged

echo "Installed mpris-bridge ${tag} for ${TARGET}."
echo "Edit ~/.config/mpris-bridge/config.toml (see examples/config/config.toml) and restart:"
echo "  systemctl --user restart mpris-bridged"