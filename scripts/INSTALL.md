# systemg Installation Script

The installation script at `scripts/index.sh` supports both latest and version-specific installations of systemg.

## Features

- **Version Management**: Install multiple versions of systemg and switch between them
- **Latest Installation**: Install the latest release by default
- **Version Switching**: Switch to already-installed versions without re-downloading
- **Live Re-execution**: Upgrade a compatible resident supervisor without restarting workloads
- **Platform Detection**: Automatic detection of OS and architecture
- **PATH Management**: Automatic PATH configuration for bash/zsh

## Directory Structure

After installation, systemg uses the following directory structure:

```
~/.local/bin/
└── sysg               # Symlink to active version
~/.sysg/
├── versions/
│   ├── 0.50.0/
│   │   └── sysg       # Version 0.50.0 binary
│   ├── 0.51.0/
│   │   └── sysg       # Version 0.51.0 binary
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
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- --version 0.51.0

# Short form
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v 0.51.0
```

### Switch to Already Installed Version

If a version is already installed, running the install command for that version will simply switch to it:

```bash
# This will switch to 0.50.0 if already installed, or install it if not
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- -v 0.50.0
```

### Upgrade a Running Supervisor

Run the normal installer. Compatible releases re-execute the supervisor in the
same PID, preserve its workloads, verify the resident target version, and only
then update the active symlink:

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

Live re-execution starts with `0.56.0` and currently applies to newer patch
releases on the same major/minor line. Installing `0.56.0` over a running
`0.55.x` supervisor requires `sysg stop --supervisor` first. An incompatible or
unsafe handoff leaves the active version unchanged and reports
[`SG0501`](https://sysg.dev/how-it-works/dialog/codes#sg0501) through
[`SG0505`](https://sysg.dev/how-it-works/dialog/codes#sg0505).

### Show Help

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh -s -- --help
```

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

The installer uses HTTPS with TLS 1.2+ for all downloads and requires the
downloaded binary to report the expected version. It validates executable
ownership and permissions before a live handoff.

## Development

To test the installer locally:

```bash
# Using a local script
cat scripts/index.sh | sh -s -- --version 0.51.0

# Or directly
sh scripts/index.sh --version 0.51.0
```
