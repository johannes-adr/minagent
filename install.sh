#!/bin/sh
# Downloads the latest release of the agent from GitHub and unpacks it.
#   ./install.sh              -> ./agent
#   DIR=~/.local/bin ./install.sh
set -eu

REPO="johannes-adr/minagent"
DIR="${DIR:-.}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)               platform="macos-arm64" ;;
  Linux-x86_64|Linux-amd64)   platform="linux-x86_64" ;;
  *) echo "No release for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
[ -n "$tag" ] || { echo "Could not determine the latest release" >&2; exit 1; }

name="agent-$tag-$platform.tar.gz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $name"
curl -fsSL -o "$tmp/$name" "https://github.com/$REPO/releases/download/$tag/$name"
curl -fsSL -o "$tmp/$name.sha256" "https://github.com/$REPO/releases/download/$tag/$name.sha256"

(cd "$tmp" && { sha256sum -c "$name.sha256" 2>/dev/null || shasum -a 256 -c "$name.sha256"; } >/dev/null) \
  || { echo "Checksum mismatch" >&2; exit 1; }

mkdir -p "$DIR"
tar -xzf "$tmp/$name" -C "$DIR"
echo "Installed $tag to $DIR/agent"
