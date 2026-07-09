#!/usr/bin/env python3
"""Stamp .claude-plugin/plugin.json with the package version from Cargo.toml."""
import json
import pathlib
import tomllib


def main():
    root = pathlib.Path(__file__).resolve().parent.parent
    cargo = tomllib.loads((root / "Cargo.toml").read_text())
    version = cargo["package"]["version"]

    path = root / ".claude-plugin" / "plugin.json"
    plugin = json.loads(path.read_text())

    if plugin.get("version") == version:
        print(f"plugin.json already at {version}")
        return

    plugin["version"] = version
    path.write_text(json.dumps(plugin, indent=2) + "\n")
    print(f"plugin.json version -> {version}")


if __name__ == "__main__":
    main()
