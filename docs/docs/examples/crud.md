---
sidebar_position: 2
title: CRUD Application
---

# CRUD Application

Production-ready Node.js API with automated testing, backups, and Slack notifications.

## Features demonstrated

- Zero-downtime rolling deployments
- Scheduled database backups
- Automated testing via cron
- Slack webhook notifications
- Environment variable management

## Configuration

```yaml
version: "1"
env:
  file: ".env.example"

services:
  node__web_server:
    command: "node server.js"
    deployment_strategy: "rolling_start"
    env:
      vars:
        NODE_ENV: "${NODE_ENV}"
        PORT: "${PORT}"
        DATABASE_URL: "${DATABASE_URL}"
    restart_policy: "on_failure"
    retries: "10"
    backoff: "10s"
    webhooks:
      on_success:
        url: "${SLACK_WEBHOOK_SUCCESS_URL}"
        method: "POST"
        timeout: "30s"
      on_failure:
        url: "${SLACK_WEBHOOK_FAILURE_URL}"
        method: "POST"
        timeout: "30s"

cron:
  automated_testing:
    command: "npm test"
    schedule: "0 */6 * * *"
    retries: "3"
    timeout: "120s"
    webhooks:
      on_failure:
        url: "${SLACK_WEBHOOK_TEST_FAILURE_URL}"
        method: "POST"

  database_backup:
    command: >
      pg_dump ${DATABASE_URL} >
      /backups/db_$(date +%Y%m%d_%H%M%S).sql
    schedule: "0 2 * * *"
    retries: "5"
    timeout: "300s"
    webhooks:
      on_success:
        url: "${SLACK_WEBHOOK_BACKUP_URL}"
        method: "POST"
      on_failure:
        url: "${SLACK_WEBHOOK_BACKUP_FAILURE_URL}"
        method: "POST"
```

## Environment file

Create `.env.example`:

```bash
NODE_ENV=production
PORT=3000
DATABASE_URL=postgres://user:pass@localhost/crud_db
SLACK_WEBHOOK_SUCCESS_URL=https://hooks.slack.com/services/xxx
SLACK_WEBHOOK_FAILURE_URL=https://hooks.slack.com/services/yyy
SLACK_WEBHOOK_TEST_FAILURE_URL=https://hooks.slack.com/services/zzz
SLACK_WEBHOOK_BACKUP_URL=https://hooks.slack.com/services/aaa
SLACK_WEBHOOK_BACKUP_FAILURE_URL=https://hooks.slack.com/services/bbb
```

## Application code

### server.js

```javascript
const express = require('express');
const app = express();
const port = process.env.PORT || 3000;

app.use(express.json());

// In-memory database for demo
let items = [];
let nextId = 1;

app.get('/items', (req, res) => {
  res.json(items);
});

app.post('/items', (req, res) => {
  const item = { id: nextId++, ...req.body };
  items.push(item);
  res.status(201).json(item);
});

app.put('/items/:id', (req, res) => {
  const id = parseInt(req.params.id);
  const index = items.findIndex(item => item.id === id);
  if (index === -1) return res.status(404).json({ error: 'Not found' });
  items[index] = { id, ...req.body };
  res.json(items[index]);
});

app.delete('/items/:id', (req, res) => {
  const id = parseInt(req.params.id);
  items = items.filter(item => item.id !== id);
  res.status(204).send();
});

app.listen(port, () => {
  console.log(`Server running on port ${port}`);
});
```

## Run it

```bash
cd examples/crud
npm install
sysg start --config crud.sysg.yaml
```

## Operations

### Deploy new version

```bash
# Update code
git pull
npm install

# Rolling restart - zero downtime
sysg restart node__web_server
```

### View logs

```bash
sysg logs node__web_server --follow
```

### Manual backup

```bash
sysg cron trigger database_backup
```

### Check cron schedules

```bash
sysg cron list
```

## What happens

1. **Web server** starts and serves API on configured port
2. **Tests** run automatically every 6 hours
3. **Backups** execute nightly at 2 AM
4. **Slack notifications** fire on:
   - Successful/failed deployments
   - Failed tests
   - Backup completion or failure
5. **Rolling deployments** ensure zero downtime during updates

## Monitoring

```bash
sysg status                           # Service health
sysg cron status                      # Cron job history
sysg logs automated_testing           # Test results
sysg logs database_backup             # Backup logs
```

## See also

- [Configuration](../how-it-works/configuration) - Service definitions
- [Cron](../how-it-works/cron) - Scheduled tasks
- [Webhooks](../how-it-works/webhooks) - Notifications
- [Rolling deployments](../how-it-works/configuration#deployment-strategies)