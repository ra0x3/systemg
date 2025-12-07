# Multi-Service Example

This example demonstrates systemg's capability to manage multiple services simultaneously with different behaviors:

- **py_size**: A continuously running Python service that monitors directory sizes
- **count_number**: A cron-scheduled task that increments a counter every 10 seconds
- **echo_lines**: A service that depends on count_number and echoes lines periodically

## Services

### py_size
- **Type**: Long-running service
- **Command**: `python3 py_size.py`
- **Restart Policy**: Always restart on failure
- **Backoff**: 5 seconds between restart attempts
- **Hooks**: Sends HTTP POST to a test endpoint on restart errors

### count_number
- **Type**: Cron job
- **Command**: `sh count_number.sh`
- **Schedule**: Every 10 seconds (`*/10 * * * * *`)
- **Restart Policy**: Never restart
- **Function**: Increments and saves a counter to `.count_state`

### echo_lines
- **Type**: Dependent service
- **Command**: `sh echo_lines.sh`
- **Depends On**: count_number
- **Restart Policy**: Never restart
- **Function**: Echoes lines from a file with delays

## Usage

### Start all services in the foreground:
```bash
sysg start
```

### Start all services as daemon:
```bash
sysg start --daemonize
```

### Check service status:
```bash
sysg status
```

### Expected Status Output:
```
Service statuses:
● [cron] count_number - Exited successfully (exit code 0)
  Cron history (UTC) for count_number:
    - 2025-12-07 19:57:40 UTC | exit 0
    - 2025-12-07 19:57:30 UTC | exit 0
    - 2025-12-07 19:57:20 UTC | exit 0

● echo_lines Running
   Active: active (running) since 00:39; 39 secs ago
 Main PID: 36275
    Tasks: 0 (limit: N/A)
   Memory: 2.7M
      CPU: 0.020s
 Process Group: 36275
     |-36275 sh echo_lines.sh
       ├─37305 sleep 5

● py_size Running
   Active: active (running) since 00:39; 39 secs ago
 Main PID: 36260
    Tasks: 0 (limit: N/A)
   Memory: 13.1M
      CPU: 0.030s
 Process Group: 36260
     |-36260 /opt/homebrew/Cellar/python@3.13/.../Python py_size.py
```

### View supervisor and service logs:
```bash
sysg logs
```

### Expected Logs Output:
```
+---------------------------------+
|           Supervisor            |
+---------------------------------+

2025-12-07T19:57:20.522297Z  INFO systemg::supervisor: Running cron job 'count_number'
2025-12-07T19:57:20.522762Z DEBUG systemg::daemon: Initializing daemon...
2025-12-07T19:57:20.522815Z  INFO systemg::daemon: Starting service: count_number
2025-12-07T19:57:20.527214Z DEBUG systemg::daemon: Service 'count_number' started with PID: 35763
2025-12-07T19:57:20.580630Z  INFO systemg::supervisor: Cron job 'count_number' completed successfully

2025-12-07T19:57:30.563509Z  INFO systemg::supervisor: Running cron job 'count_number'
2025-12-07T19:57:30.566047Z DEBUG systemg::daemon: Service 'count_number' started with PID: 35824
2025-12-07T19:57:30.621800Z  INFO systemg::supervisor: Cron job 'count_number' completed successfully
```

### Stop all services:
```bash
sysg stop
```

## Files Generated

- `.count_state`: Stores the current counter value
- `.count_start`: Stores the initial counter value
- `echo.txt`: Output from echo_lines service
- `lines.txt`: Input data for echo_lines service

## Configuration

See `systemg.yaml` for the complete service definitions.
