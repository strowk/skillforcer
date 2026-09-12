#!/bin/sh
# skillforcer installer for macOS, Linux, and Windows Git Bash.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/strowk/skillforcer/main/install.sh | sh
#
# Environment:
#   SKILLFORCER_VERSION      release tag to install (default: latest)
#   SKILLFORCER_INSTALL_DIR  install directory (default: $HOME/.local/bin)
#   GITHUB_TOKEN / GH_TOKEN  token with 'repo' scope (required while the repo is private)
set -eu

REPO="strowk/skillforcer"
BIN="skillforcer"
INSTALL_DIR="${SKILLFORCER_INSTALL_DIR:-$HOME/.local/bin}"
API="https://api.github.com/repos/$REPO"

TOKEN="${GITHUB_TOKEN:-${GH_TOKEN:-}}"

err() {
  echo "skillforcer install: $1" >&2
  exit 1
}

command -v curl >/dev/null 2>&1 || err "curl is required"

# Resolve target triple from OS + architecture.
os=$(uname -s)
arch=$(uname -m)
ext=""
case "$os" in
  Linux)
    case "$arch" in
      x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
      aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
      *) err "unsupported Linux architecture: $arch" ;;
    esac
    ;;
  Darwin)
    case "$arch" in
      x86_64 | amd64) target="x86_64-apple-darwin" ;;
      arm64 | aarch64) target="aarch64-apple-darwin" ;;
      *) err "unsupported macOS architecture: $arch" ;;
    esac
    ;;
  MINGW* | MSYS* | CYGWIN* | Windows_NT)
    case "$arch" in
      x86_64 | amd64) target="x86_64-pc-windows-msvc"; ext=".exe" ;;
      *) err "unsupported Windows architecture: $arch (only x86_64 is published)" ;;
    esac
    ;;
  *) err "unsupported OS: $os" ;;
esac

asset="skillforcer-${target}${ext}"

# Build the auth header (needed while the repo is private).
auth=""
if [ -n "$TOKEN" ]; then
  auth="Authorization: Bearer $TOKEN"
fi

api_get() {
  # $1 = url. Emits the response body.
  if [ -n "$auth" ]; then
    curl -fsSL -H "$auth" -H "Accept: application/vnd.github+json" "$1"
  else
    curl -fsSL -H "Accept: application/vnd.github+json" "$1"
  fi
}

# Resolve the release (a specific tag or the latest).
if [ -n "${SKILLFORCER_VERSION:-}" ]; then
  release_url="$API/releases/tags/$SKILLFORCER_VERSION"
else
  release_url="$API/releases/latest"
fi

release_json=$(api_get "$release_url") ||
  err "could not fetch release metadata (private repo? set GITHUB_TOKEN with 'repo' scope)"

tag=$(printf '%s' "$release_json" | grep -o '"tag_name":[ ]*"[^"]*"' | head -n1 | sed -e 's/.*"tag_name":[ ]*"//' -e 's/"$//')
[ -n "$tag" ] || err "no releases found for $REPO"

# Extract the asset's API URL (works for private repos; browser URLs need a web session).
# The response is pretty-printed, so track the most recent "url" line and emit it once
# the matching asset "name" line is reached (url precedes name within each asset object).
asset_url=$(printf '%s' "$release_json" | awk -v tgt="\"$asset\"" '
  /"url":/ { u = $0 }
  index($0, "\"name\":") && index($0, tgt) { print u; exit }
' | sed -e 's/.*"url":[ ]*"//' -e 's/".*//')
[ -n "$asset_url" ] || err "release $tag has no asset named $asset"

echo "Installing skillforcer $tag ($target) to $INSTALL_DIR"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
out="$tmp/$asset"

if [ -n "$auth" ]; then
  curl -fL -H "$auth" -H "Accept: application/octet-stream" "$asset_url" -o "$out" ||
    err "download failed"
else
  curl -fL -H "Accept: application/octet-stream" "$asset_url" -o "$out" ||
    err "download failed"
fi

mkdir -p "$INSTALL_DIR"
dest="$INSTALL_DIR/${BIN}${ext}"
mv "$out" "$dest"
chmod +x "$dest"

echo "Installed: $dest"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "Note: $INSTALL_DIR is not on your PATH. Add it, e.g.:"
     echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

echo "Run 'skillforcer install' inside a project to register the hooks."
