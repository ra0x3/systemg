# systemg Installation Script

The installation script at `scripts/index.sh` supports both latest and version-specific installations of systemg.

## Features

- **Version Management**: Install multiple versions of systemg and switch between them
- **Latest Installation**: Install the latest release by default
- **Version Switching**: Switch to already-installed versions without re-downloading
- **Platform Detection**: Automatic detection of OS and architecture
- **PATH Management**: Automatic PATH configuration for bash/zsh

## Directory Structure

After installation, systemg uses the following directory structure:

```
~/.sysg/
├── bin/
│   └── sysg           # Symlink to active version
├── versions/
│   ├── 0.15.5/
│   │   └── sysg       # Version 0.15.5 binary
│   ├── 0.15.6/
│   │   └── sysg       # Version 0.15.6 binary
│   └── ...
└── active-version     # File containing active version number
```

## Usage

### Install Latest Version

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

### Install Specific Version

```bash
# Long form
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- --version 0.15.6

# Short form
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v 0.15.6
```

### Switch to Already Installed Version

If a version is already installed, running the install command for that version will simply switch to it:

```bash
# This will switch to 0.15.5 if already installed, or install it if not
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v 0.15.5
```

### Show Help

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- --help
```

## Environment Variables

- `SYSG_INSTALL_ALLOW_VERSION_MISMATCH=1` - Allow installation even if the downloaded binary reports a different version than expected

## Platform Support

The installer supports the following platforms:

### Linux
- `x86_64-unknown-linux-gnu` (with Debian variant detection)
- `aarch64-unknown-linux-gnu`

### macOS
- `x86_64-apple-darwin` (Intel)
- `aarch64-apple-darwin` (Apple Silicon)

## Troubleshooting

### Version Not Found

If a specific version is not available for your platform, the installer will show an error and direct you to the releases page:
https://github.com/ra0x3/systemg/releases

### PATH Not Updated

If the installer cannot automatically update your PATH, you'll need to manually add:

```bash
export PATH="$HOME/.sysg/bin:$PATH"
```

to your shell configuration file (`~/.bashrc`, `~/.zshrc`, etc.)

### Switching Versions

To see all installed versions and switch between them, you can:

1. List installed versions:
   ```bash
   ls ~/.sysg/versions/
   ```

2. Switch to a specific version:
   ```bash
   curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v VERSION
   ```

## Security

The installer uses HTTPS with TLS 1.2+ for all downloads and verifies that the downloaded binary reports the expected version before installation.

## Development

To test the installer locally:

```bash
# Using a local script
cat scripts/index.sh | sh -s -- --version 0.15.6

# Or directly
sh scripts/index.sh --version 0.15.6
```