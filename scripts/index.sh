#!/bin/sh
set -e

REQUESTED_VERSION=""
while [ $# -gt 0 ]; do
  case "$1" in
    --version|-v)
      shift
      if [ -z "${1:-}" ]; then
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

SYSG_ROOT="$HOME/.sysg"
SYSG_BIN_DIR="$SYSG_ROOT/bin"
SYSG_VERSIONS_DIR="$SYSG_ROOT/versions"
SYSG_ACTIVE_VERSION_FILE="$SYSG_ROOT/active-version"

mkdir -p "$SYSG_BIN_DIR" "$SYSG_VERSIONS_DIR"

if [ "$OS" = "linux" ]; then
  if [ "$ARCH" = "x86_64" ]; then
    TARGET="x86_64-unknown-linux-gnu"
    if [ -r /etc/os-release ]; then
      . /etc/os-release
      case "${ID:-}:${ID_LIKE:-}" in
        debian:*|*:debian*|debian:debian*) TARGET="x86_64-unknown-linux-gnu-debian" ;;
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

echo "Setting up systemg..."
echo ""

LATEST_VERSION=""
fetch_latest() {
  curl -s https://api.github.com/repos/ra0x3/systemg/releases/latest \
    | awk -F'"' '/tag_name/ {gsub(/^v/, "", $4); print $4}'
}

if [ -n "$REQUESTED_VERSION" ]; then
  VERSION="${REQUESTED_VERSION#v}"
  LATEST_VERSION="$(fetch_latest)"
  if [ -z "$LATEST_VERSION" ]; then
    LATEST_VERSION="$VERSION"
  fi
else
  VERSION="$(fetch_latest)"
  if [ -z "$VERSION" ]; then
    echo "❌ Failed to determine latest version from GitHub"
    exit 1
  fi
  LATEST_VERSION="$VERSION"
fi

VERSION_DIR="$SYSG_VERSIONS_DIR/$VERSION"
VERSION_BINARY="$VERSION_DIR/sysg"

CURRENT_ACTIVE_VERSION=""
if [ -f "$SYSG_ACTIVE_VERSION_FILE" ]; then
  CURRENT_ACTIVE_VERSION=$(cat "$SYSG_ACTIVE_VERSION_FILE" 2>/dev/null || echo "")
fi

if [ -x "$VERSION_BINARY" ]; then
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

TEMP_DIR=$(mktemp -d 2>/dev/null || mktemp -d -t 'sysg-install')
ORIGINAL_DIR="$PWD"
cd "$TEMP_DIR"

tar -xzf "$ORIGINAL_DIR/$FILE"
rm "$ORIGINAL_DIR/$FILE"

if [ -f "sysg" ]; then
  BINARY="sysg"
elif [ -d "sysg-$VERSION-$TARGET" ] && [ -f "sysg-$VERSION-$TARGET/sysg" ]; then
  BINARY="sysg-$VERSION-$TARGET/sysg"
else
  FOUND="$(find . -maxdepth 2 -type f -name sysg | head -n 1)"
  if [ -n "$FOUND" ]; then
    BINARY="$FOUND"
  else
    echo "❌ sysg binary not found after extraction"
    cd "$ORIGINAL_DIR"
    rm -rf "$TEMP_DIR"
    exit 1
  fi
fi

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
    cd "$ORIGINAL_DIR"
    rm -rf "$TEMP_DIR"
    exit 1
  fi
fi

mkdir -p "$VERSION_DIR"
mv "$BINARY" "$VERSION_BINARY"

ln -sf "$VERSION_BINARY" "$SYSG_BIN_DIR/sysg"
echo "$VERSION" > "$SYSG_ACTIVE_VERSION_FILE"

cd "$ORIGINAL_DIR"
rm -rf "$TEMP_DIR"

PATH_LINE='export PATH="$HOME/.sysg/bin:$PATH"'
SHELL_RC=""

if [ -n "${BASH_VERSION:-}" ]; then
  SHELL_RC="$HOME/.bashrc"
elif [ -n "${ZSH_VERSION:-}" ]; then
  SHELL_RC="$HOME/.zshrc"
elif echo "${SHELL:-}" | grep -q "bash"; then
  SHELL_RC="$HOME/.bashrc"
elif echo "${SHELL:-}" | grep -q "zsh"; then
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

# ---- UI ----
if [ -t 1 ]; then
  BOLD="$(printf '\033[1m')"
  DIM="$(printf '\033[2m')"
  RESET="$(printf '\033[0m')"

  C_BORDER_MAIN="$(printf '\033[38;2;96;96;96m')"      # ~ #606060
  C_BORDER_SUB="$(printf '\033[38;2;168;208;248m')"    # ~ #A8D0F8
  C_LOGO="$(printf '\033[38;2;0;159;255m')"            # ~ #009FFF
  C_TEXT="$(printf '\033[38;2;248;248;248m')"          # ~ #F8F8F8
  C_MUTED="$(printf '\033[38;2;96;96;96m')"            # ~ #606060
  C_MUTED2="$(printf '\033[38;2;136;136;136m')"        # ~ #888888
  C_GREEN="$(printf '\033[38;2;176;200;151m')"         # ~ #B0C897
else
  BOLD=""; DIM=""; RESET=""
  C_BORDER_MAIN=""; C_BORDER_SUB=""; C_LOGO=""; C_TEXT=""; C_MUTED=""; C_MUTED2=""; C_GREEN=""
fi

term_cols() {
  if [ -r /dev/tty ]; then
    stty size </dev/tty 2>/dev/null | awk '{print $2; exit}' && return
  fi
  if command -v tput >/dev/null 2>&1; then
    tput cols 2>/dev/null && return
  fi
  stty size 2>/dev/null | awk '{print $2; exit}' && return
  echo 80
}

# Box inner width is 78 characters (between the │ borders)
BOX_INNER=78

# Helper to center text within the box
center_text() {
  text="$1"
  len=${#text}
  total_pad=$((BOX_INNER - len))
  left_pad=$((total_pad / 2))
  right_pad=$((total_pad - left_pad))
  printf "%*s%s%*s" "$left_pad" "" "$text" "$right_pad" ""
}

# Helper to create a labeled row: "Label:    Value" with proper padding
label_row() {
  label="$1"
  value="$2"
  left_margin=20
  label_width=12
  content="${label}$(printf '%*s' $((label_width - ${#label})) '')${value}"
  content_len=${#content}
  right_pad=$((BOX_INNER - left_margin - content_len))
  printf "%*s%s%*s" "$left_margin" "" "$content" "$right_pad" ""
}

WIDTH=94
COLS="$(term_cols)"
if [ "$COLS" -gt "$WIDTH" ]; then
  PAD=$(( (COLS - WIDTH) / 2 ))
else
  PAD=0
fi

p() { printf "%*s%s\n" "$PAD" "" "$1"; }

# Calculate dynamic padding for version string
VERSION_LABEL="systemg ${VERSION}"
VERSION_CENTERED="$(center_text "$VERSION_LABEL")"

echo ""
p "${C_BORDER_MAIN}╭──────────────────────────────────────────────────────────────────────────────╮${RESET}"
p "${C_BORDER_MAIN}│                                                                              │${RESET}"
p "${C_BORDER_MAIN}│                                                                              │${RESET}"
p "${C_BORDER_MAIN}│                               ${C_LOGO}█▀▀ █▄█ █▀▀ ▀█▀ █▀▀ █▀▄▀█ █▀▀${C_BORDER_MAIN}                  │${RESET}"
p "${C_BORDER_MAIN}│                               ${C_LOGO}▄█  █  ▄█    █  █▄▄ █ ▀ █ █▄█${C_BORDER_MAIN}                  │${RESET}"
p "${C_BORDER_MAIN}│                                                                              │${RESET}"
p "${C_BORDER_MAIN}│${C_TEXT}${BOLD}${VERSION_CENTERED}${RESET}${C_BORDER_MAIN}│${RESET}"
p "${C_BORDER_MAIN}│                                                                              │${RESET}"
p "${C_BORDER_MAIN}│$(label_row "Docs:" "https://sysg.dev")│${RESET}"
p "${C_BORDER_MAIN}│$(label_row "Releases:" "github.com/ra0x3/systemg/releases")│${RESET}"
p "${C_BORDER_MAIN}│$(label_row "Support:" "github.com/ra0x3/systemg/issues")│${RESET}"
p "${C_BORDER_MAIN}│                                                                              │${RESET}"
p "${C_BORDER_MAIN}╰──────────────────────────────────────────────────────────────────────────────╯${RESET}"

p ""
p "${C_BORDER_SUB}╭──────────────────────────────────────────────────────────────────────────────╮${RESET}"
p "${C_BORDER_SUB}│${C_TEXT}${BOLD}$(center_text "systemg 1.0 is coming!")${RESET}${C_BORDER_SUB}│${RESET}"
p "${C_BORDER_SUB}│${C_MUTED2}${DIM}$(center_text "A general-purpose program composer")${RESET}${C_BORDER_SUB}│${RESET}"
p "${C_BORDER_SUB}╰──────────────────────────────────────────────────────────────────────────────╯${RESET}"

if [ -n "$LATEST_VERSION" ] && [ "$LATEST_VERSION" != "$VERSION" ]; then
  p ""
  UPDATE_LABEL="Update available: ${LATEST_VERSION}"
  UPDATE_CMD="Run: curl -fsSL https://sh.sysg.dev | sh"
  p "${C_BORDER_SUB}╭──────────────────────────────────────────────────────────────────────────────╮${RESET}"
  p "${C_BORDER_SUB}│${C_TEXT}${BOLD}$(center_text "$UPDATE_LABEL")${RESET}${C_BORDER_SUB}│${RESET}"
  p "${C_BORDER_SUB}│${C_MUTED2}${DIM}$(center_text "$UPDATE_CMD")${RESET}${C_BORDER_SUB}│${RESET}"
  p "${C_BORDER_SUB}╰──────────────────────────────────────────────────────────────────────────────╯${RESET}"
fi

PATH_NEEDS_UPDATE=0
case ":$PATH:" in
  *":$HOME/.sysg/bin:"*) ;;
  *) PATH_NEEDS_UPDATE=1 ;;
esac

if [ $PATH_NEEDS_UPDATE -eq 1 ]; then
  echo ""
  echo "⚠ Setup notes:"
  if [ -n "$SHELL_RC" ] && grep -q ".sysg/bin" "$SHELL_RC"; then
    echo "  • Path configuration added to $SHELL_RC but not yet loaded. Run:"
    echo ""
    echo "    . \"$SHELL_RC\""
  else
    echo "  • ~/.sysg/bin is not in your PATH. Run:"
    echo ""
    echo "    echo 'export PATH=\"\$HOME/.sysg/bin:\$PATH\"' >> ~/.bashrc && . ~/.bashrc"
  fi
  echo ""
fi

echo ""
echo "✅ Installation complete!"
