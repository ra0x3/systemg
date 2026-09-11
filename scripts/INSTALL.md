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

Live re-execution starts with `0.56.0`. From `0.57.1` forward, a strictly newer
release can upgrade across version lines when its live-reexec protocol and
handoff schema match the resident. Residents from `0.56.0` through `0.57.0`
enforce the original same-major/minor rule; earlier residents do not support
live re-execution. An incompatible or unsafe handoff leaves the active version
unchanged and reports
[`SG0501`](https://sysg.dev/reference/dialog/codes#sg0501) through
[`SG0505`](https://sysg.dev/reference/dialog/codes#sg0505).

For [`SG0502`](https://sysg.dev/reference/dialog/codes#sg0502), stop the
supervisor, rerun the installer, then restart each required project:

```bash
sysg stop --supervisor
curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

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

The installer picks your shell from `$SHELL`. For bash it puts this line at the
top of `~/.bashrc`. For zsh it puts it at the top of `~/.zshenv` and adds it to
`~/.zshrc`:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

The line goes at the top because most `~/.bashrc` files stop early for
non-interactive shells, and a one-shot `ssh host 'sysg status'` runs one of
those. Older installers added the line at the bottom. Rerun the installer to fix
an existing host, even if it's already on the latest version.

If the installer fell back to `~/.sysg/bin`, the line uses that directory
instead. For any other shell, add the directory to `PATH` yourself using that
shell's syntax.

### Cron and systemd

Cron and systemd don't read shell startup files, so call sysg by its absolute
path, such as `/home/ubuntu/.local/bin/sysg` (or `/home/ubuntu/.sysg/bin/sysg`
if the installer fell back). Debian and Ubuntu cron default to
`PATH=/usr/bin:/bin`. A systemd user unit can use `%h/.local/bin/sysg`.

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
