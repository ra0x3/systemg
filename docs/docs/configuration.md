---
sidebar_position: 4
title: Configuration
---

Below is an example of a complete `systemg` configuration file.


```yaml
# Configuration file version
version: 1

services:
  # Name of the service
  postgres:
    # The command to start the service
    command: "postgres -D /var/lib/postgres"

    env:
      # Path to a file containing environment variables for this service
      file: "/etc/myapp/database.env"

    # Policy for restarting the service: "always", "on-failure", or "never"
    restart_policy: "always"

    # Time to wait before attempting a restart (backoff duration)
    backoff: "5s"

    hooks:
      # Command to run when the service starts successfully
      on_start: "echo 'Postgres started'"
      # Command to run if the service crashes or fails to start
      on_error: "echo 'Postgres crashed'"

  django:
    command: "python manage.py runserver"

    env:
      vars:
        # Inline environment variables for the service
        DEBUG: "true"
        DATABASE_URL: "postgres://user:password@localhost:5432/dbname"

    restart_policy: "on-failure"
    backoff: "5s"

    # List of services this one depends on (must be started before this)
    depends_on:
      - "postgres"

    hooks:
      on_start: "curl -X POST http://example.com/hook/django-start"
      on_error: "curl -X POST http://example.com/hook/django-error"

  ngrok:
    command: "ngrok http 8000"
    restart_policy: "on-failure"
    backoff: "3s"

    hooks:
      on_start: "echo 'ngrok started'"
      on_error: "echo 'ngrok crashed'"
```