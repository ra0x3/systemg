#!/bin/sh
set -e

ARCH=$(uname -m)
OS=$(uname -s | tr '[:upper:]' '[:lower:]')

if [ "$OS" = "darwin" ]; then
  FILE="systemg-macos.tar.gz"
elif [ "$OS" = "linux" ]; then
  FILE="systemg-linux.tar.gz"
else
  echo "Unsupported OS: $OS"
  exit 1
fi

curl -L "https://sh.sysg.dev/$FILE" | tar xz
chmod +x systemg
sudo mv systemg /usr/local/bin/
echo "✅ systemg installed successfully!"
