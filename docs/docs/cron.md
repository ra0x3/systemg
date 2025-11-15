---
sidebar_position: 5
title: Cron Scheduling
---

# Cron Scheduling

Systemg supports cron-based scheduling for short-lived, recurring tasks. This feature is perfect for periodic jobs like backups, cleanups, data synchronization, or any task that needs to run on a regular schedule.

## Overview

Cron jobs in systemg are:
- **Managed by the supervisor** - The supervisor checks for due jobs every second and executes them automatically.
- **Short-lived** - Cron jobs are designed to run, complete their task, and exit. They are not meant to be long-running services.
- **Scheduled** - Jobs run according to a cron expression that defines when they should execute.
- **Independent** - Each execution is independent and cleaned up after completion.

## Configuration

To configure a service as a cron job, add a `cron` block to the service definition:

```yaml
version: "1"
services:
  backup:
    command: "sh /scripts/backup.sh"
    cron:
      expression: "0 0 * * * *"  # Run every hour at minute 0
      timezone: "America/New_York"  # Optional, defaults to system timezone
```

### Cron Fields

The `cron` configuration block supports the following fields:

- **`expression`** *(required)* - A 6-field cron expression that defines when the job should run.
- **`timezone`** *(optional)* - The timezone to use for scheduling. Defaults to the system timezone.

### Cron Expression Format

Systemg uses a 6-field cron expression format:

```
┌───────────── second (0-59)
│ ┌───────────── minute (0-59)
│ │ ┌───────────── hour (0-23)
│ │ │ ┌───────────── day of month (1-31)
│ │ │ │ ┌───────────── month (1-12)
│ │ │ │ │ ┌───────────── day of week (0-6, Sunday = 0)
│ │ │ │ │ │
* * * * * *
```

### Examples

Run every minute:
```yaml
cron:
  expression: "0 * * * * *"
```

Run every day at midnight:
```yaml
cron:
  expression: "0 0 0 * * *"
```

Run every Monday at 9:00 AM:
```yaml
cron:
  expression: "0 0 9 * * 1"
```

Run every 30 minutes:
```yaml
cron:
  expression: "0 */30 * * * *"
```

Run on the 1st and 15th of each month at noon:
```yaml
cron:
  expression: "0 0 12 1,15 * *"
```

## Important Notes

### Service Separation

A service **cannot** have both a regular `command` execution and a `cron` configuration at the same time. The presence of a `cron` block explicitly opts the service into cron-style scheduling.

**This is invalid:**
```yaml
services:
  myservice:
    command: "myapp"
    restart_policy: "always"  # This doesn't make sense for cron jobs
    cron:
      expression: "0 * * * * *"
```

**This is valid:**
```yaml
services:
  # Regular long-running service
  web:
    command: "python app.py"
    restart_policy: "always"

  # Cron-scheduled task
  cleanup:
    command: "sh cleanup.sh"
    cron:
      expression: "0 0 2 * * *"  # Run daily at 2 AM
```

### Overlap Detection

If a cron job is scheduled to run while the previous execution is still running, systemg will:

1. **Skip the new execution** - The scheduled run will not start.
2. **Log an error** - A warning message indicating the overlap will be logged.
3. **Record the overlap** - The execution history will show an "overlap error" for that scheduled run.

This prevents multiple instances of the same job from running simultaneously, which could cause conflicts or resource contention.

**Example:**
```
[WARN] Cron job 'backup' is scheduled to run but previous execution is still running
```

### Execution History

Systemg tracks the last 10 executions of each cron job, including:
- **Start time** - When the job began executing.
- **Completion time** - When the job finished (or `None` if still running).
- **Status** - One of:
  - `Success` - Job completed successfully (exit code 0).
  - `Failed(error)` - Job failed with an error message.
  - `OverlapError` - Job was skipped due to overlap with previous execution.

You can view this history using the `sysg status` command (see Status Display below).

### No Restart Policies

Cron jobs do not support restart policies (`restart_policy`, `backoff`, etc.). They are designed to:
1. Run when scheduled
2. Complete their task
3. Exit
4. Clean up

If a cron job fails, it will be recorded in the execution history, but it will not be automatically restarted. The job will run again on its next scheduled time.

### Dependencies

Cron jobs can still use the `depends_on` field, but this is generally **not recommended** because:
- Cron jobs are short-lived and independent.
- Dependencies are designed for long-running services that need other services to be available.

If you need to ensure certain conditions are met before a cron job runs, handle that logic within the job's command itself.

## Status Display

When you run `sysg status`, cron jobs are displayed differently from regular services:

```
Active services:
● web - Running
  Active: active (running) since Tue 2025-11-14 10:30:00; 2 hours ago
  Main PID: 12345
  ...

Scheduled cron jobs:
● backup
  Schedule: 0 0 * * * * (every hour)
  Next run: Tue 2025-11-14 13:00:00 (in 10 mins)
  Last run: Tue 2025-11-14 12:00:00 (success)
  Recent executions:
    - 2025-11-14 12:00:00 → 12:00:05 [Success]
    - 2025-11-14 11:00:00 → 11:00:04 [Success]
    - 2025-11-14 10:00:00 → 10:00:06 [Success]
```

## Use Cases

Cron scheduling in systemg is ideal for:

- **Backups** - Run database or file backups on a regular schedule.
- **Cleanup tasks** - Remove old logs, temporary files, or expired data.
- **Data synchronization** - Periodically fetch data from external sources.
- **Report generation** - Generate and send reports at specific times.
- **Health checks** - Run periodic health checks and alert on failures.
- **Batch processing** - Process queued items or batch operations.

## Limitations

- **No timezone-aware scheduling** - While you can specify a timezone, the cron scheduler uses UTC internally. Timezone support may vary.
- **Minimum 1-second granularity** - The supervisor checks for due jobs every second, so sub-second scheduling is not supported.
- **No distributed locking** - If you run multiple systemg instances with the same configuration, each will execute the cron jobs independently.

## Examples

### Daily Database Backup

```yaml
version: "1"
services:
  db-backup:
    command: "pg_dump mydb > /backups/mydb-$(date +%Y%m%d).sql"
    cron:
      expression: "0 0 2 * * *"  # 2 AM daily
```

### Hourly Log Rotation

```yaml
version: "1"
services:
  rotate-logs:
    command: "sh /scripts/rotate-logs.sh"
    cron:
      expression: "0 0 * * * *"  # Every hour
```

### Weekly Report Generation

```yaml
version: "1"
services:
  weekly-report:
    command: "python generate-report.py --week"
    cron:
      expression: "0 0 9 * * 1"  # Mondays at 9 AM
```

### Every 5 Minutes Health Check

```yaml
version: "1"
services:
  health-check:
    command: "curl -f http://myservice/health || exit 1"
    cron:
      expression: "0 */5 * * * *"  # Every 5 minutes
```

## Best Practices

1. **Keep jobs short** - Cron jobs should complete quickly. For long-running tasks, consider using a regular service instead.

2. **Handle errors gracefully** - Make sure your cron job commands exit with appropriate error codes (0 for success, non-zero for failure).

3. **Use absolute paths** - Specify full paths to scripts and binaries to avoid issues with working directory.

4. **Test your expressions** - Use a cron expression tester to verify your schedule before deploying.

5. **Monitor execution history** - Regularly check `sysg status` to ensure your cron jobs are running as expected.

6. **Avoid overlaps** - Design your tasks to complete well before the next scheduled run.

7. **Use environment variables** - Leverage the `env` configuration to pass settings to your cron jobs.

## Troubleshooting

### Job not running

- Verify the cron expression is correct using a cron expression tester.
- Check the supervisor logs for errors: `sysg logs`.
- Ensure the supervisor is running: `sysg status`.

### Overlap errors

- Increase the time between runs to allow jobs to complete.
- Optimize your job to run faster.
- Consider using a lock file within your script to prevent concurrent runs.

### Failed executions

- Check the execution history in `sysg status` for error messages.
- Test your command manually to identify issues.
- Ensure all dependencies and resources are available.
