---
sidebar_position: 2
title: CRUD Application
---

# CRUD Application Example

This example demonstrates how to use `systemg` to manage a production-ready Node.js CRUD application with automated testing, database backups, rolling deployments, and Slack notifications.

## Overview

This example showcases advanced `systemg` features:
- **Zero-downtime deployments** with `rolling_start` strategy
- **Scheduled cron jobs** for automated testing and database backups
- **Webhook notifications** to Slack for operational awareness
- **Environment variable management** for secure configuration
- **Comprehensive restart policies** for service reliability

## Application Setup

The example includes a standard Node.js/Express application with:
- REST API with CRUD endpoints (`GET`, `POST`, `PUT`, `DELETE`)
- PostgreSQL database integration
- Automated test suite
- Database backup scripts

The application code itself is intentionally simple to keep the focus on `systemg` configuration.

## Configuration File

The complete `systemg` configuration is in `crud.sysg.yaml`:

### Environment Configuration

```yaml
# Use the v1 config schema
version: "1"

# Load shared environment variables for every service and cron job
env:
  # Read variables from the example .env file
  file: ".env.example"
```

**What this does:**
- **`version: "1"`**: Specifies the configuration schema version
- **`env.file`**: Loads all environment variables from `.env.example` file, making them available to all services and cron jobs

### Web Server Service

```yaml
# Define the web server managed by systemg
services:
  node__web_server:
    # Start the Node.js API entry point
    command: "node server.js"
    # Roll traffic to the new process before stopping the old one
    deployment_strategy: "rolling_start"
    env:
      # Pass through deployment-specific variables
      vars:
        # Application environment (development/staging/production)
        NODE_ENV: "${NODE_ENV}"
        # HTTP port exposed by the server
        PORT: "${PORT}"
        # PostgreSQL connection string for the app
        DATABASE_URL: "${DATABASE_URL}"
    # Automatically restart on non-zero exit codes
    restart_policy: "on_failure"
    # Allow up to ten restart attempts
    retries: "10"
    # Wait ten seconds between retries
    backoff: "10s"
```

**Configuration breakdown:**

- **`node__web_server`**: Service identifier used in commands like `sysg logs node__web_server`
- **`command: "node server.js"`**: The command to execute to start the service
- **`deployment_strategy: "rolling_start"`**: Enables zero-downtime deployments by:
  1. Starting the new version of the service
  2. Waiting for it to become healthy
  3. Stopping the old version
  4. This ensures no dropped requests during deployments
- **`env.vars`**: Service-specific environment variables that override or supplement the global env
  - **`NODE_ENV`**: Application environment (development, staging, production)
  - **`PORT`**: The port the web server listens on
  - **`DATABASE_URL`**: PostgreSQL connection string
- **`restart_policy: "on_failure"`**: Automatically restart if the service exits with non-zero status
- **`retries: "10"`**: Maximum number of restart attempts before marking service as failed
- **`backoff: "10s"`**: Wait 10 seconds between restart attempts to avoid rapid restart loops

### Deployment Webhooks

```yaml
    # Notify Slack about deployment outcomes
    webhooks:
      on_success:
        # Webhook endpoint for successful rollouts
        url: "${SLACK_WEBHOOK_SUCCESS_URL}"
        method: "POST"
        headers:
          # Send JSON payloads to Slack
          Content-Type: "application/json"
        body: |
          {
            "text": "✅ CRUD API deployment successful - Web server is running",
            ...
          }
      on_error:
        # Webhook endpoint for failed rollouts
        url: "${SLACK_WEBHOOK_ERROR_URL}"
        method: "POST"
        headers:
          # Slack expects JSON content here as well
          Content-Type: "application/json"
        body: |
          {
            "text": "❌ CRUD API deployment failed - Web server encountered an error",
            ...
          }
```

**Configuration breakdown:**

- **`webhooks.on_success`**: Triggered when the service successfully starts
  - **`url`**: Slack webhook URL for success notifications
  - **`method: "POST"`**: HTTP method for the webhook request
  - **`headers`**: HTTP headers, here specifying JSON content
  - **`body`**: JSON payload sent to Slack with deployment details
- **`webhooks.on_error`**: Triggered when the service fails to start
  - Sends critical alerts to a different Slack channel or with mentions (`<!channel>`)
  - Enables immediate response to deployment failures

### Automated Test Suite (Cron Job)

```yaml
# Schedule recurring jobs alongside services
cron:
  test_suite:
    # Run the test suite at the top of every hour
    schedule: "0 * * * *"
    # Execute the package.json test script
    command: "npm test"
    env:
      # Override environment for the cron context
      vars:
        # Point to the isolated test database
        DATABASE_URL: "${TEST_DATABASE_URL}"
        # Force Node into test mode
        NODE_ENV: "test"
```

**Configuration breakdown:**

- **`test_suite`**: Identifier for this cron job
- **`schedule: "0 * * * *"`**: Cron expression meaning "run at minute 0 of every hour"
  - Format: `minute hour day month weekday`
  - This runs hourly: 00:00, 01:00, 02:00, etc.
- **`command: "npm test"`**: Executes the test suite defined in package.json
- **`env.vars`**: Job-specific environment variables
  - **`DATABASE_URL`**: Points to test database instead of production
  - **`NODE_ENV: "test"`**: Ensures tests run in test mode

The test suite also includes `on_success` and `on_error` webhooks to notify the team of test results.

### Database Backup (Cron Job)

```yaml
  database_backup:
    # Take a snapshot every six hours
    schedule: "0 */6 * * *"
    # Run the backup script that dumps and uploads the database
    command: "bash scripts/backup-database.sh"
    env:
      # Provide credentials scoped to the backup job
      vars:
        # Production database connection string
        DATABASE_URL: "${DATABASE_URL}"
        # Target S3 bucket for dump files
        BACKUP_S3_BUCKET: "${BACKUP_S3_BUCKET}"
        # AWS key used for uploading backups
        AWS_ACCESS_KEY_ID: "${AWS_ACCESS_KEY_ID}"
        # AWS secret paired with the access key
        AWS_SECRET_ACCESS_KEY: "${AWS_SECRET_ACCESS_KEY}"
        # Number of days to retain historical backups
        BACKUP_RETENTION_DAYS: "${BACKUP_RETENTION_DAYS}"
```

**Configuration breakdown:**

- **`database_backup`**: Identifier for the backup job
- **`schedule: "0 */6 * * *"`**: Cron expression meaning "run every 6 hours"
  - Executes at: 00:00, 06:00, 12:00, 18:00 daily
  - Provides 4 backup points per day for disaster recovery
- **`command`**: Runs the backup script that creates PostgreSQL dump and uploads to S3
- **`env.vars`**: Credentials and configuration for backup operations
  - **`DATABASE_URL`**: Database to back up
  - **`BACKUP_S3_BUCKET`**: S3 bucket name for storing backups
  - **`AWS_ACCESS_KEY_ID`** and **`AWS_SECRET_ACCESS_KEY`**: AWS credentials for S3 access
  - **`BACKUP_RETENTION_DAYS`**: How long to keep old backups before deletion

Critical `on_success` and `on_error` webhooks notify the team of backup status, with `on_error` using `<!channel>` for urgent alerts.

## Environment Variables

The `.env.example` file defines all required variables:

```bash
# Application Configuration
NODE_ENV=
PORT=

# Database Configuration
DATABASE_URL=
TEST_DATABASE_URL=

# Backup Configuration
BACKUP_S3_BUCKET=
AWS_ACCESS_KEY_ID=
AWS_SECRET_ACCESS_KEY=
BACKUP_RETENTION_DAYS=

# Slack Webhook URLs for Notifications
SLACK_WEBHOOK_SUCCESS_URL=
SLACK_WEBHOOK_ERROR_URL=
```

**What each variable controls:**

- **`NODE_ENV`**: Sets the application environment (development, staging, production)
- **`PORT`**: Port number for the web server (e.g., 3000, 8080)
- **`DATABASE_URL`**: PostgreSQL connection string (e.g., `postgresql://user:pass@localhost:5432/dbname`)
- **`TEST_DATABASE_URL`**: Separate database for running tests
- **`BACKUP_S3_BUCKET`**: S3 bucket name (e.g., `my-app-backups`)
- **`AWS_ACCESS_KEY_ID`** and **`AWS_SECRET_ACCESS_KEY`**: AWS credentials with S3 write permissions
- **`BACKUP_RETENTION_DAYS`**: Days to retain old backups (e.g., 30)
- **`SLACK_WEBHOOK_SUCCESS_URL`**: Slack incoming webhook for success notifications
- **`SLACK_WEBHOOK_ERROR_URL`**: Slack incoming webhook for error/critical alerts

## Running the Example

1. **Setup environment**:
   ```bash
   cd examples/crud
   cp .env.example .env
   # Edit .env with your actual values
   ```

2. **Install dependencies**:
   ```bash
   npm install
   ```

3. **Start with systemg**:
   ```bash
   sysg start
   ```

4. **Check status**:
   ```bash
   sysg status
   ```

5. **View logs**:
   ```bash
   sysg logs node__web_server
   ```

6. **Test the API**:
   ```bash
   curl http://localhost:3000/api/items
   ```

## Key Features Demonstrated

### 1. Rolling Deployments

When you update your code and restart the service, `rolling_start` ensures:
- No connection errors for active users
- Graceful transition between versions
- Automatic rollback if new version fails health checks

### 2. Automated Testing

Hourly test execution helps:
- Catch issues early, even in production
- Monitor API health continuously
- Get immediate alerts if tests fail

### 3. Database Backups

Regular automated backups ensure:
- Protection against data loss
- Compliance with backup policies
- Point-in-time recovery capabilities
- Automated cleanup of old backups

### 4. Operational Notifications

Slack webhooks provide:
- Real-time deployment status
- Test suite results
- Backup completion confirmations
- Critical alerts for failures

## What You Learned

In this example, you learned how to:
- ✅ Implement zero-downtime deployments with `rolling_start`
- ✅ Schedule automated tasks using cron syntax
- ✅ Configure webhook notifications for operational events
- ✅ Manage environment variables securely
- ✅ Set up comprehensive restart policies
- ✅ Separate concerns between services and scheduled jobs
- ✅ Build production-ready service configurations

## Next Steps

- Customize the webhook payloads for your notification system
- Add more cron jobs for other operational tasks (log rotation, cache clearing, etc.)
- Implement health check endpoints for better rolling deployment detection
- Add monitoring and alerting for service metrics
- Explore other `systemg` features in the [documentation](/docs/configuration)
