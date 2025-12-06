#!/bin/sh
set -e

ARCH=$(uname -m)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')

# -----------------------------
# Detect platform + target triple
# -----------------------------
if [ "$OS" = "linux" ]; then
  if [ "$ARCH" = "x86_64" ]; then
    TARGET="x86_64-unknown-linux-gnu"

    # Detect Debian/Ubuntu
    if [ -r /etc/os-release ]; then
      # shellcheck disable=SC1091
      . /etc/os-release
      case "${ID:-}:${ID_LIKE:-}" in
        debian:*|*:debian*|debian:debian*)
          TARGET="x86_64-unknown-linux-gnu-debian"
          ;;
      esac
    fi

  elif [ "$ARCH" = "aarch64" ] || [ "$ARCH" = "arm64" ]; then
    TARGET="aarch64-unknown-linux-gnu"
  else
    echo "Unsupported architecture: $ARCH"
    exit 1
  fi

elif [ "$OS" = "darwin" ]; then
  if [ "$ARCH" = "x86_64" ]; then
    TARGET="x86_64-apple-darwin"
  elif [ "$ARCH" = "arm64" ]; then
    TARGET="aarch64-apple-darwin"
  else
    echo "Unsupported architecture: $ARCH"
    exit 1
  fi
else
  echo "Unsupported OS: $OS"
  exit 1
fi

# -----------------------------
# Fetch version
# -----------------------------
echo "Fetching latest version..."
VERSION=$(
  curl -s https://api.github.com/repos/ra0x3/systemg/releases/latest \
    | awk -F'"' '/tag_name/ {gsub(/^v/, "", $4); print $4}'
)

if [ -z "$VERSION" ]; then
  echo "Failed to determine latest version from GitHub."
  exit 1
fi

# -----------------------------
# Check if already installed
# -----------------------------
INSTALL_DIR="$HOME/.sysg/bin"
EXISTING_BINARY=""

if [ -x "$INSTALL_DIR/sysg" ]; then
  EXISTING_BINARY="$INSTALL_DIR/sysg"
elif command -v sysg >/dev/null 2>&1; then
  EXISTING_BINARY=$(command -v sysg)
fi

if [ -n "$EXISTING_BINARY" ]; then
  CURRENT_VERSION=$(
    "$EXISTING_BINARY" --version 2>/dev/null \
      | awk 'NR==1 {print $2; exit}' \
      | sed 's/^v//' || true
  )

  if [ "$CURRENT_VERSION" = "$VERSION" ]; then
    echo "sysg $VERSION is already installed and up to date."
    exit 0
  elif [ -n "$CURRENT_VERSION" ]; then
    echo "Upgrading sysg managed at $EXISTING_BINARY from $CURRENT_VERSION to $VERSION..."
  else
    echo "Installing sysg $VERSION (existing version could not be determined)."
  fi
fi

FILE="sysg-$VERSION-$TARGET.tar.gz"
URL="https://sh.sysg.dev/$FILE"

echo "Downloading sysg $VERSION for $TARGET..."
if ! curl -sSfL "$URL" -o "$FILE"; then
  echo "Binary '$FILE' not available for your platform."
  echo "Available releases: https://github.com/ra0x3/systemg/releases"
  exit 1
fi

# -----------------------------
# Extract
# -----------------------------
tar -xzf "$FILE"
rm "$FILE"

# Find the sysg binary after extraction
if [ -f "sysg" ]; then
  BINARY="sysg"
elif [ -d "sysg-$VERSION-$TARGET" ] && [ -f "sysg-$VERSION-$TARGET/sysg" ]; then
  BINARY="sysg-$VERSION-$TARGET/sysg"
else
  # Fallback: search for a sysg binary
  FOUND="$(find . -maxdepth 2 -type f -name sysg | head -n 1)"
  if [ -n "$FOUND" ]; then
    BINARY="$FOUND"
  else
    echo "Error: sysg binary not found after extraction."
    exit 1
  fi
fi

# -----------------------------
# Verify and install
# -----------------------------
chmod +x "$BINARY"

RESOLVED_BINARY="$BINARY"
case "$BINARY" in
  /*) ;;
  *) RESOLVED_BINARY="./$BINARY" ;;
esac

DOWNLOADED_VERSION=$(
  "$RESOLVED_BINARY" --version 2>/dev/null \
    | awk 'NR==1 {print $2; exit}' \
    | sed 's/^v//' || true
)

if [ -n "$DOWNLOADED_VERSION" ] && [ "$DOWNLOADED_VERSION" != "$VERSION" ]; then
  echo "Downloaded sysg reports version $DOWNLOADED_VERSION (expected $VERSION)." >&2
  if [ "${SYSG_INSTALL_ALLOW_VERSION_MISMATCH:-}" = "1" ]; then
    echo "Continuing install because SYSG_INSTALL_ALLOW_VERSION_MISMATCH=1." >&2
  else
    echo "Aborting install; please verify release artifacts or rerun with SYSG_INSTALL_ALLOW_VERSION_MISMATCH=1." >&2
    rm -rf "sysg-$VERSION-$TARGET" "$BINARY" 2>/dev/null || true
    exit 1
  fi
fi

mkdir -p "$INSTALL_DIR"

mv "$BINARY" "$INSTALL_DIR/sysg"

# Clean extraction directory safely
rm -rf "sysg-$VERSION-$TARGET" 2>/dev/null || true

# -----------------------------
# Shell PATH update
# -----------------------------
PATH_LINE='export PATH="$HOME/.sysg/bin:$PATH"'
SHELL_RC=""

if [ -n "$BASH_VERSION" ]; then
  SHELL_RC="$HOME/.bashrc"
elif [ -n "$ZSH_VERSION" ]; then
  SHELL_RC="$HOME/.zshrc"
elif echo "$SHELL" | grep -q "bash"; then
  SHELL_RC="$HOME/.bashrc"
elif echo "$SHELL" | grep -q "zsh"; then
  SHELL_RC="$HOME/.zshrc"
fi

if [ -n "$SHELL_RC" ]; then
  mkdir -p "$(dirname "$SHELL_RC")"
  touch "$SHELL_RC"

  if ! grep -q ".sysg/bin" "$SHELL_RC"; then
    {
      echo ""
      echo "# Added by sysg installer"
      echo "$PATH_LINE"
    } >> "$SHELL_RC"
    echo "Updated PATH in $SHELL_RC"
  fi
fi

export PATH="$HOME/.sysg/bin:$PATH"

echo ""
echo "sysg $VERSION installed successfully to $INSTALL_DIR"
echo ""
echo "To start using sysg:"
echo "  - Restart your terminal, OR"
echo "  - Run: export PATH=\"\$HOME/.sysg/bin:\$PATH\""
echo ""
echo "Run 'sysg --help' to get started."
