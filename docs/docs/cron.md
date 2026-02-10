---
sidebar_position: 5
title: Cron Scheduling
---

# Cron Scheduling

Schedule recurring tasks with cron expressions.

## Configuration

```yaml
services:
  backup:
    command: "sh /scripts/backup.sh"
    cron:
      expression: "0 0 * * * *"  # Every hour
      timezone: "America/New_York"  # Optional
```

## Format

6-field cron: `second minute hour day month weekday`

```
0 * * * * *      # Every minute
0 0 0 * * *      # Daily at midnight
0 0 9 * * 1      # Mondays at 9am
0 */30 * * * *   # Every 30 minutes
0 0 12 1,15 * *  # 1st and 15th at noon
```

## Notes

- Cron jobs are short-lived (run and exit)
- Cannot mix with `restart_policy`
- Overlap detection prevents duplicate runs

## Examples

```yaml
services:
  # Database backup daily at 2am
  db-backup:
    command: "pg_dump mydb > /backups/mydb-$(date +%Y%m%d).sql"
    cron:
      expression: "0 0 2 * * *"

  # Health check every 5 minutes
  health-check:
    command: "curl -f http://myservice/health"
    cron:
      expression: "0 */5 * * * *"
```

## Status

```
Scheduled cron jobs:
● backup
  Schedule: 0 0 * * * * (every hour)
  Next run: 13:00:00 (in 10 mins)
  Last run: 12:00:00 (success)
```
