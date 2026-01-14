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
        echo "❌ --version requires a version number"
        echo ""
        echo "  Usage: curl ... | sh -s -- --version VERSION"
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
      echo ""
      echo "Options:"
      echo "  --version, -v VERSION    Install or activate a specific version"
      echo "  --help, -h               Show this help message"
      echo ""
      echo "Examples:"
      echo "  curl ... | sh                        # Install latest version"
      echo "  curl ... | sh -s -- --version 0.15.6 # Install version 0.15.6"
      exit 0
      ;;
    *)
      echo "❌ Unknown option: $1"
      echo ""
      echo "  Use --help for usage information"
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

# -----------------------------
# Determine version to install
# -----------------------------
echo "Setting up systemg..."
echo ""

if [ -n "$REQUESTED_VERSION" ]; then
  # User specified a version
  VERSION="$REQUESTED_VERSION"
  # Remove 'v' prefix if present
  VERSION="${VERSION#v}"
else
  # Fetch latest version
  VERSION=$(
    curl -s https://api.github.com/repos/ra0x3/systemg/releases/latest \
      | awk -F'"' '/tag_name/ {gsub(/^v/, "", $4); print $4}'
  )

  if [ -z "$VERSION" ]; then
    echo "❌ Failed to determine latest version from GitHub"
    exit 1
  fi
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
      echo "✔ sysg $VERSION is already installed and active"
      echo ""
      echo "  Run: sysg --help to get started"
      echo ""
      echo "✅ Setup complete!"
      exit 0
    else
      echo "$VERSION" > "$SYSG_ACTIVE_VERSION_FILE"
      ln -sf "$VERSION_BINARY" "$SYSG_BIN_DIR/sysg"
      echo "✔ Switched to sysg $VERSION"
      echo ""
      echo "  Run: sysg --help to get started"
      echo ""
      echo "✅ Setup complete!"
      exit 0
    fi
  else
    rm -rf "$VERSION_DIR"
  fi
fi

FILE="sysg-$VERSION-$TARGET.tar.gz"
URL="https://sh.sysg.dev/$FILE"

if ! curl -sSfL "$URL" -o "$FILE" 2>/dev/null; then
  echo "❌ Binary '$FILE' not available for your platform"
  echo ""
  echo "  Available releases: https://github.com/ra0x3/systemg/releases"
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
    echo "❌ sysg binary not found after extraction"
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
  if [ "${SYSG_INSTALL_ALLOW_VERSION_MISMATCH:-}" != "1" ]; then
    echo "❌ Version mismatch detected (got $DOWNLOADED_VERSION, expected $VERSION)" >&2
    echo "" >&2
    echo "  To continue anyway: SYSG_INSTALL_ALLOW_VERSION_MISMATCH=1" >&2
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
  fi
fi

export PATH="$HOME/.sysg/bin:$PATH"

echo ""
echo "✔ systemg successfully installed!"
echo ""
echo "  Version: $VERSION"
echo ""
echo "  Location: $HOME/.sysg/bin/sysg"
echo ""
echo ""
echo "  Next: Run sysg --help to get started"

# Check if PATH needs to be updated
PATH_NEEDS_UPDATE=0
case ":$PATH:" in
  *":$HOME/.sysg/bin:"*)
    # Already in PATH
    ;;
  *)
    PATH_NEEDS_UPDATE=1
    ;;
esac

if [ $PATH_NEEDS_UPDATE -eq 1 ]; then
  echo ""
  echo "⚠ Setup notes:"
  if [ -n "$SHELL_RC" ] && grep -q ".sysg/bin" "$SHELL_RC"; then
    echo "  • Path configuration added to $SHELL_RC but not yet loaded. Run:"
    echo ""
    echo "  source $SHELL_RC"
  else
    echo "  • ~/.sysg/bin is not in your PATH. Run:"
    echo ""
    echo "  echo 'export PATH=\"\$HOME/.sysg/bin:\$PATH\"' >> ~/.bashrc && source ~/.bashrc"
  fi
  echo ""
fi

echo ""
echo "✅ Installation complete!"
