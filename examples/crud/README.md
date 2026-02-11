# CRUD Application Example

This example demonstrates how to use `systemg` to manage a production-ready Node.js CRUD application with automated testing, database backups, and deployment notifications.

## Overview

This example showcases advanced `systemg` features for managing a realistic web application:

- **Web Server Management**: Run a Node.js/Express API server as a managed service
- **Rolling Deployments**: Zero-downtime deployments using `rolling_start` strategy
- **Scheduled Testing**: Automated hourly test suite execution via cron
- **Database Backups**: Automated backups every 6 hours with S3 storage
- **Webhook Notifications**: Slack alerts for deployment and operational events
- **Environment Management**: Secure configuration using environment variables

## What's Included

This is a minimal Node.js CRUD application with:

- **Express.js** web server with REST API endpoints
- **PostgreSQL** database for data persistence
- **Automated tests** to ensure API reliability
- **Database backup script** for data safety
- **systemg configuration** for complete lifecycle management

## Setup

### 1. Install Dependencies

```bash
npm install
```

### 2. Configure Environment

Copy the example environment file and fill in your values:

```bash
cp .env.example .env
```

Edit `.env` and provide:
- Database connection strings
- AWS S3 credentials for backups
- Slack webhook URLs for notifications
- Application port and environment

### 3. Database Setup

Create your PostgreSQL database and run migrations:

```bash
npm run migrate
```

## Running with systemg

### Start All Services

Start the web server and enable all scheduled jobs:

```bash
sysg start
```

This will:
- Start the web server with rolling deployment strategy
- Enable hourly automated testing
- Enable 6-hourly database backups
- Send success/error notifications to Slack

### Check Status

View the status of all services and cron jobs:

```bash
sysg status
```

### View Logs

View logs for the web server:

```bash
sysg logs --service node__web_server
```

### Stop Services

```bash
sysg stop
```

## Configuration Highlights

### Rolling Deployments

```yaml
deployment_strategy: "rolling_start"
```

When you update your application, `systemg` will:
1. Start the new version
2. Wait for it to be healthy
3. Stop the old version
4. Ensure zero downtime

### Automated Testing

```yaml
cron:
  test_suite:
    schedule: "0 * * * *"  # Every hour
    command: "npm test"
```

Tests run automatically every hour and send notifications on success/failure.

### Database Backups

```yaml
cron:
  database_backup:
    schedule: "0 */6 * * *"  # Every 6 hours
    command: "bash scripts/backup-database.sh"
```

Automated backups run 4 times per day (00:00, 06:00, 12:00, 18:00) and upload to S3.

### Webhook Notifications

Both the web server and cron jobs send Slack notifications:
- **on_success**: Sent when services start successfully or jobs complete
- **on_error**: Sent when services fail or jobs encounter errors

## Project Structure

```
crud/
├── server.js                  # Express.js web server
├── routes/                    # API route handlers
├── models/                    # Database models
├── tests/                     # Test suite
├── scripts/
│   └── backup-database.sh    # Database backup script
├── package.json              # Node.js dependencies
├── .env.example              # Environment variable template
├── crud.sysg.yaml            # systemg configuration
└── README.md                 # This file
```

## API Endpoints

The CRUD API provides standard RESTful endpoints:

- `GET /api/items` - List all items
- `GET /api/items/:id` - Get a specific item
- `POST /api/items` - Create a new item
- `PUT /api/items/:id` - Update an item
- `DELETE /api/items/:id` - Delete an item

## Monitoring

With `systemg`, you get built-in monitoring:

1. **Service Health**: Automatic restart on failure with configurable backoff
2. **Deployment Notifications**: Know immediately if a deployment succeeds or fails
3. **Scheduled Job Status**: Slack notifications for test and backup results
4. **Logs**: Centralized logging with `sysg logs` command

## Best Practices Demonstrated

- ✅ Zero-downtime deployments with rolling strategy
- ✅ Separation of configuration from code using environment variables
- ✅ Automated testing for continuous quality assurance
- ✅ Regular database backups for disaster recovery
- ✅ Real-time operational notifications via webhooks
- ✅ Proper restart policies with exponential backoff

## Learn More

For more information about `systemg` features used in this example, see:

- [Configuration Reference](https://docs.systemg.dev/docs/configuration)
- [Cron Jobs](https://docs.systemg.dev/docs/cron)
- [Webhooks](https://docs.systemg.dev/docs/webhooks)
