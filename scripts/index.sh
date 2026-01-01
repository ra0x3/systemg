#!/bin/sh
set -e

# -----------------------------
# Parse command line arguments
# -----------------------------
REQUESTED_VERSION=""
while [ $# -gt 0 ]; do
  case "$1" in
    --version|-v)
      shift
      if [ -z "$1" ]; then
        echo "Error: --version requires a version number"
        echo "Usage: curl ... | sh -s -- --version VERSION"
        echo "       curl ... | sh -s -- -v VERSION"
        exit 1
      fi
      REQUESTED_VERSION="$1"
      shift
      ;;
    --help|-h)
      echo "systemg installer"
      echo ""
      echo "Usage:"
      echo "  Install latest:     curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh"
      echo "  Install specific:   curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- --version VERSION"
      echo "  Install specific:   curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v VERSION"
      echo ""
      echo "Options:"
      echo "  --version, -v VERSION    Install or activate a specific version"
      echo "  --help, -h               Show this help message"
      echo ""
      echo "Examples:"
      echo "  curl ... | sh                        # Install latest version"
      echo "  curl ... | sh -s -- --version 0.15.6 # Install version 0.15.6"
      echo "  curl ... | sh -s -- -v 0.15.6        # Install version 0.15.6 (short form)"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      echo "Use --help for usage information"
      exit 1
      ;;
  esac
done

ARCH=$(uname -m)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')

# -----------------------------
# Setup directory structure
# -----------------------------
SYSG_ROOT="$HOME/.sysg"
SYSG_BIN_DIR="$SYSG_ROOT/bin"
SYSG_VERSIONS_DIR="$SYSG_ROOT/versions"
SYSG_ACTIVE_VERSION_FILE="$SYSG_ROOT/active-version"

mkdir -p "$SYSG_BIN_DIR"
mkdir -p "$SYSG_VERSIONS_DIR"

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
# Determine version to install
# -----------------------------
if [ -n "$REQUESTED_VERSION" ]; then
  # User specified a version
  VERSION="$REQUESTED_VERSION"
  # Remove 'v' prefix if present
  VERSION="${VERSION#v}"
  echo "Installing specified version: $VERSION"
else
  # Fetch latest version
  echo "Fetching latest version..."
  VERSION=$(
    curl -s https://api.github.com/repos/ra0x3/systemg/releases/latest \
      | awk -F'"' '/tag_name/ {gsub(/^v/, "", $4); print $4}'
  )

  if [ -z "$VERSION" ]; then
    echo "Failed to determine latest version from GitHub."
    exit 1
  fi
  echo "Latest version: $VERSION"
fi

# -----------------------------
# Check if version is already installed
# -----------------------------
VERSION_DIR="$SYSG_VERSIONS_DIR/$VERSION"
VERSION_BINARY="$VERSION_DIR/sysg"

# Check current active version
CURRENT_ACTIVE_VERSION=""
if [ -f "$SYSG_ACTIVE_VERSION_FILE" ]; then
  CURRENT_ACTIVE_VERSION=$(cat "$SYSG_ACTIVE_VERSION_FILE" 2>/dev/null || echo "")
fi

# If this version is already installed
if [ -x "$VERSION_BINARY" ]; then
  # Verify the installed binary actually reports the correct version
  INSTALLED_VERSION=$(
    "$VERSION_BINARY" --version 2>/dev/null \
      | awk 'NR==1 {print $2; exit}' \
      | sed 's/^v//' || true
  )

  if [ "$INSTALLED_VERSION" = "$VERSION" ]; then
    if [ "$CURRENT_ACTIVE_VERSION" = "$VERSION" ]; then
      echo "sysg $VERSION is already installed and active."
      exit 0
    else
      echo "sysg $VERSION is already installed. Switching to it..."
      echo "$VERSION" > "$SYSG_ACTIVE_VERSION_FILE"
      ln -sf "$VERSION_BINARY" "$SYSG_BIN_DIR/sysg"
      echo "Switched to sysg $VERSION"
      exit 0
    fi
  else
    echo "Warning: Installed binary reports version $INSTALLED_VERSION (expected $VERSION)."
    echo "Re-downloading..."
    rm -rf "$VERSION_DIR"
  fi
fi

echo "Installing sysg $VERSION..."

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
# Create a temporary directory for extraction
TEMP_DIR=$(mktemp -d 2>/dev/null || mktemp -d -t 'sysg-install')
cd "$TEMP_DIR"

tar -xzf "$OLDPWD/$FILE"
rm "$OLDPWD/$FILE"

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
    cd "$OLDPWD"
    rm -rf "$TEMP_DIR"
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
    cd "$OLDPWD"
    rm -rf "$TEMP_DIR"
    exit 1
  fi
fi

# Create version-specific directory and install
mkdir -p "$VERSION_DIR"
mv "$BINARY" "$VERSION_BINARY"

# Create/update symlink to active version
ln -sf "$VERSION_BINARY" "$SYSG_BIN_DIR/sysg"

# Mark this version as active
echo "$VERSION" > "$SYSG_ACTIVE_VERSION_FILE"

# Clean up
cd "$OLDPWD"
rm -rf "$TEMP_DIR"

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
echo "sysg $VERSION installed successfully!"
echo ""
echo "Installation details:"
echo "  Active version: $VERSION"
echo "  Binary location: $VERSION_BINARY"
echo "  Symlink: $SYSG_BIN_DIR/sysg -> $VERSION_BINARY"
echo ""

# Show list of installed versions
echo "Installed versions:"
for version_path in "$SYSG_VERSIONS_DIR"/*; do
  if [ -d "$version_path" ] && [ -x "$version_path/sysg" ]; then
    version_name=$(basename "$version_path")
    if [ "$version_name" = "$VERSION" ]; then
      echo "  * $version_name (active)"
    else
      echo "    $version_name"
    fi
  fi
done
echo ""

echo "To start using sysg:"
echo "  - Restart your terminal, OR"
echo "  - Run: export PATH=\"\$HOME/.sysg/bin:\$PATH\""
echo ""
echo "To switch versions later:"
echo "  curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- --version VERSION"
echo ""
echo "Run 'sysg --help' to get started."
