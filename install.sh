#!/bin/sh
# Installs craft-mcp-hub (crafthub and crafthub-panel) on macOS or Linux, or updates it.
#
#   curl -fsSL https://raw.githubusercontent.com/mayor686/craft-mcp-hub/main/install.sh | sh
#
# The programs go to ~/.local/bin (set CRAFTHUB_INSTALL_DIR for another folder). On macOS the control
# panel is also installed as the app "Craft MCP Hub" in /Applications (or ~/Applications), where
# Launchpad and Spotlight find it, and the programs in that folder point into it.
set -eu

REPO="mayor686/craft-mcp-hub"
BIN_DIR="${CRAFTHUB_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) echo "Unsupported system: $(uname -s). Build from source: https://github.com/$REPO#build-from-source" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) echo "Unsupported processor: $(uname -m). Build from source: https://github.com/$REPO#build-from-source" >&2; exit 1 ;;
esac
if [ "$os-$arch" = "linux-aarch64" ]; then
  echo "There is no ready-made build for Linux on ARM yet. Build from source: https://github.com/$REPO#build-from-source" >&2
  exit 1
fi

asset="crafthub-$os-$arch.tar.gz"
base="https://github.com/$REPO/releases/latest/download"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $asset..."
for file in "$asset" "$asset.sha256"; do
  if ! curl -fsSL --proto '=https' --tlsv1.2 "$base/$file" -o "$tmp/$file"; then
    echo "Could not download $base/$file" >&2
    exit 1
  fi
done

# Nothing is installed unless the download is the file the release was built with.
expected=$(awk '{ print $1; exit }' "$tmp/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$asset" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')
else
  echo "Neither sha256sum nor shasum is here to check the download with; nothing was installed." >&2
  exit 1
fi
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  echo "The download does not match its checksum; nothing was installed." >&2
  exit 1
fi

tar -xzf "$tmp/$asset" -C "$tmp"

updating=no
if [ -x "$BIN_DIR/crafthub" ]; then
  updating=yes
fi
mkdir -p "$BIN_DIR"
for bin in crafthub crafthub-panel; do
  if [ -f "$tmp/$bin" ]; then
    # Removed first: on macOS it may be a link into the app, which must not be written through.
    rm -f "$BIN_DIR/$bin"
    install -m 755 "$tmp/$bin" "$BIN_DIR/$bin"
  fi
done

if [ "$os" = macos ]; then
  if ! "$BIN_DIR/crafthub" install-app --link "$BIN_DIR"; then
    echo "The app could not be put in Applications; the programs in $BIN_DIR work all the same." >&2
  fi
fi

echo "Installed in $BIN_DIR: $("$BIN_DIR/crafthub" --version)"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Add it to your PATH to run it by name:  export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
echo
if [ "$updating" = yes ]; then
  echo "Servers that are running keep the previous version until they are restarted:"
  echo "       crafthub restart all    (this closes what their engines have open)"
else
  echo "Next:  crafthub setup    (adds and turns on every installed ArtCraft app)"
  if [ "$os" = macos ]; then
    echo "       Open \"Craft MCP Hub\" from Applications, Launchpad or Spotlight (or: crafthub panel)"
  else
    echo "       crafthub panel    (opens the control panel)"
  fi
fi
