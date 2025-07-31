#!/bin/sh
set -e

ARCH=$(uname -m)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')

if [ "$OS" = "linux" ]; then
  if [ "$ARCH" = "x86_64" ]; then
    TARGET="x86_64-unknown-linux-gnu"
  elif [ "$ARCH" = "aarch64" ] || [ "$ARCH" = "arm64" ]; then
    TARGET="aarch64-unknown-linux-gnu"
  else
    echo "❌ Unsupported architecture: $ARCH"
    exit 1
  fi
elif [ "$OS" = "darwin" ]; then
  if [ "$ARCH" = "x86_64" ]; then
    TARGET="x86_64-apple-darwin"
  elif [ "$ARCH" = "arm64" ]; then
    TARGET="aarch64-apple-darwin"
  else
    echo "❌ Unsupported architecture: $ARCH"
    exit 1
  fi
else
  echo "❌ Unsupported OS: $OS"
  exit 1
fi

echo "🔍 Fetching latest version..."
VERSION=$(curl -s https://api.github.com/repos/ra0x3/systemg/releases/latest | grep '"tag_name"' | cut -d'"' -f4 | sed 's/^v//')

if [ -z "$VERSION" ]; then
  echo "❌ Failed to fetch latest version"
  exit 1
fi

FILE="sysg-$VERSION-$TARGET.tar.gz"
echo "📦 Downloading sysg $VERSION for $TARGET..."

if ! curl -sSfL "https://sh.sysg.dev/$FILE" -o "$FILE"; then
  echo "❌ Binary not available for $TARGET"
  echo "Available binaries: https://github.com/ra0x3/systemg/releases"
  exit 1
fi

tar -xzf "$FILE"
rm "$FILE"

if [ -f "sysg" ]; then
  BINARY="sysg"
elif [ -f "sysg-$VERSION-$TARGET/sysg" ]; then
  BINARY="sysg-$VERSION-$TARGET/sysg"
else
  echo "❌ Binary not found after extraction"
  exit 1
fi

mkdir -p "$HOME/.sysg/bin"
chmod +x "$BINARY"
mv "$BINARY" "$HOME/.sysg/bin/sysg"
rm -rf sysg-* 2>/dev/null || true

SHELL_RC=""
if [ -n "$BASH_VERSION" ]; then
  SHELL_RC="$HOME/.bashrc"
elif [ -n "$ZSH_VERSION" ]; then
  SHELL_RC="$HOME/.zshrc"
elif [ "$SHELL" = "/bin/bash" ]; then
  SHELL_RC="$HOME/.bashrc"
elif [ "$SHELL" = "/bin/zsh" ] || [ "$SHELL" = "/usr/bin/zsh" ]; then
  SHELL_RC="$HOME/.zshrc"
fi

PATH_LINE='export PATH="$HOME/.sysg/bin:$PATH"'

if [ -n "$SHELL_RC" ] && [ -f "$SHELL_RC" ]; then
  if ! grep -q ".sysg/bin" "$SHELL_RC"; then
    echo "" >> "$SHELL_RC"
    echo "# Added by sysg installer" >> "$SHELL_RC"
    echo "$PATH_LINE" >> "$SHELL_RC"
    echo "📝 Added $HOME/.sysg/bin to PATH in $SHELL_RC"
  fi
fi

echo "✅ sysg installed successfully to $HOME/.sysg/bin!"
echo ""
echo "To use sysg, either:"
echo "  1. Restart your shell, or"
echo "  2. Run: source $SHELL_RC"
echo "  3. Run: export PATH=\"\$HOME/.sysg/bin:\$PATH\""
echo ""
echo "Then run 'sysg --help' to get started."
