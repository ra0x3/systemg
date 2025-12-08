# Hello World Example

A simple systemg service that continuously prints numbered lines to demonstrate basic service management.

## Configuration

The `hello-world.sysg.yaml` configuration file defines the service:

```yaml
version: "1"
services:
  sh__hello_world:
    command: "sh hello-world.sh"
    env:
      file: ".env"
      vars:
        FOO: "foo"
    restart_policy: "on_failure"
    retries: "5"
    backoff: "5s"
```

## Service Configuration

- **Name**: sh__hello_world
- **Command**: `sh hello-world.sh`
- **Restart Policy**: Restart on failure only
- **Max Retries**: 5 attempts
- **Backoff**: 5 seconds between restart attempts
- **Environment**: Loads variables from `.env` file and defines `FOO=foo`

## Usage

### Start the service:
```bash
sysg start
```

### Start as daemon:
```bash
sysg start --daemonize
```

### Check status:
```bash
sysg status
```

### Expected output:
```
● sh__hello_world Running
   Active: active (running) since 12:34; 10 secs ago
 Main PID: 12345
    Tasks: 0 (limit: N/A)
   Memory: 2.5M
      CPU: 0.015s
 Process Group: 12345
     |-12345 sh hello-world.sh
```

### View logs:
```bash
sysg logs sh__hello_world
```

### Expected log output:
```
Line number: 1
Line number: 2
Line number: 3
...
```

### Stop the service:
```bash
sysg stop
```

## Script Behavior

The `hello-world.sh` script:
1. Initializes a counter at 1
2. Continuously prints "Line number: X" where X increments
3. Sleeps for 2 seconds between each print
4. Runs indefinitely until stopped
