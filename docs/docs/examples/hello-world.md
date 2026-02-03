---
sidebar_position: 1
title: Hello World
---

# Hello World Example

This example demonstrates the basic functionality of `systemg` by running a simple shell script that prints incrementing line numbers.

## Overview

The Hello World example shows you how to:
- Define a basic service
- Configure environment variables
- Set up restart policies for fault tolerance

## Files

The example consists of two files:

### Shell Script (`hello-world.sh`)

A simple script that prints incrementing numbers every 2 seconds:

```bash
#!/bin/sh

i=1

while true; do
    echo "Line number: $i"
    i=$((i + 1))
    sleep 2
done
```

### Configuration (`hello-world.sysg.yaml`)

The `systemg` configuration file that defines how to run the service:

```yaml
# Use the v1 config schema
version: "1"

# Declare the managed services table
services:
  sh__hello_world:
    # Run the shell script using the system shell
    command: "sh hello-world.sh"
    env:
      # Load environment variables from a .env file if present
      file: ".env"
      vars:
        # Inline environment variable override for the service
        FOO: "foo"
    # Restart only when the process exits with a failure code
    restart_policy: "on_failure"
    # Attempt up to five restarts before giving up
    retries: "5"
    # Wait five seconds between restart attempts
    backoff: "5s"
```

## Configuration Breakdown

Let's examine each part of the configuration:

### Service Name

```yaml
services:
  sh__hello_world:
```

The service is named `sh__hello_world`. Service names should be descriptive and unique within your configuration.

### Command

```yaml
command: "sh hello-world.sh"
```

This directive tells `systemg` which command to execute. In this case, it runs our shell script using the `sh` interpreter.

### Environment Variables

```yaml
env:
  file: ".env"
  vars:
    FOO: "foo"
```

Environment variables can be configured in two ways:
- **`file`**: Load environment variables from a `.env` file
- **`vars`**: Define inline environment variables directly in the configuration

The service will have access to both the variables from the `.env` file (if it exists) and the inline `FOO` variable.

### Restart Policy

```yaml
restart_policy: "on_failure"
retries: "5"
backoff: "5s"
```

These directives configure how `systemg` handles service failures:
- **`restart_policy: "on_failure"`**: Automatically restart the service if it exits with a non-zero status code
- **`retries: "5"`**: Attempt to restart the service up to 5 times before giving up
- **`backoff: "5s"`**: Wait 5 seconds between restart attempts to avoid rapid restart loops

## Running the Example

1. Navigate to the example directory:
   ```bash
   cd examples/hello-world
   ```

2. Create a `.env` file (optional):
   ```bash
   echo "EXAMPLE_VAR=value" > .env
   ```

3. Start the service:
   ```bash
   sysg start
   ```

4. Check the service status:
   ```bash
   sysg status
   ```

5. View the logs:
   ```bash
   sysg logs sh__hello_world
   ```

   You should see output like:
   ```
   Line number: 1
   Line number: 2
   Line number: 3
   ...
   ```

6. Stop the service:
   ```bash
   sysg stop
   ```

## What You Learned

In this example, you learned how to:
- ✅ Create a basic `systemg` configuration file
- ✅ Run a shell script as a managed service
- ✅ Configure environment variables
- ✅ Set up automatic restart policies for fault tolerance
- ✅ Use basic `systemg` commands to manage services

## Next Steps

Ready for a more realistic example? Check out the [CRUD Application example](/docs/examples/crud) to see how `systemg` can manage a full web application with cron jobs, database backups, and webhook notifications.
