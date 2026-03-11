# Units Examples

This folder documents useful command-based units you can stage with `sysg start`
without maintaining a full project config file.

## Quick examples

### HTTP file server

```bash
sysg start --daemonize --name docs-server -- python3 -m http.server 8080
```

### Log tailing

```bash
sysg start --daemonize --name app-tail -- tail -F /var/log/app.log
```

### Periodic heartbeat

```bash
sysg start --daemonize --name heartbeat -- sh -lc 'while true; do date; sleep 30; done'
```

## Unit files

Generated YAML files are stored in:

```bash
~/.local/share/systemg/units/
```

## Restart behavior

If the supervisor is already running, starting a new unit command stages a YAML
file and prints an explicit restart command. Restart is never implicit.

Example:

```bash
sysg restart --daemonize --config ~/.local/share/systemg/units/<unit>.yaml
```
