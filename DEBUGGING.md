# Debugging systemg

systemg is a concurrent supervisor with three kinds of state at once:

- live OS processes and process groups
- supervisor memory and IPC
- persisted PID, lifecycle, cron, and log state

A plausible explanation from only one layer is not enough. Debugging succeeds
when the same timeline is supported by the CLI, supervisor log, process table,
and persisted state.

## Ground rules

1. Treat the live stack as the source of truth.
2. Start read-only. Do not restart, stop, kill, purge, edit a manifest, or open a
   dependency tunnel until the evidence has been captured.
3. Separate observations from inferences.
4. Verify the exact binary under test before trusting any result.
5. Write down the expected state transition before changing code.
6. Change one causal boundary at a time.
7. Use Docker use cases as a regression net, not proof that a live workflow is
   correct.

The two high-value live stacks are documented in
[`tests/docker/usecase/LIVE_STACK.md`](tests/docker/usecase/LIVE_STACK.md).
Never edit their manifests or application files while debugging systemg unless
the owner explicitly requests it.

## Establish the binary first

Testing the wrong executable invalidates everything that follows.

```sh
command -v sysg
sysg --version
cat ~/.sysg/active-version
readlink ~/.sysg/bin/sysg
readlink ~/.local/bin/sysg
~/.sysg/versions/0.56.0/sysg --version
```

When validating a newly built symbol or message, inspect the versioned binary:

```sh
strings ~/.sysg/versions/0.56.0/sysg | rg -F 'No units being supervised'
```

Do not hide installer output. A command such as this can make a failed install
look successful while PATH still points at an older binary:

Do not discard installer output while validating a release:

```sh
./scripts/test-local.sh --version 0.56.0 >/dev/null
```

Capture its exit code and full output instead. Confirm the PATH version again
afterward.

### Example: the reported version was not the tested version

During the 0.55 investigation, a local install command reported an active older
version while `sysg --version` still printed the newer broken version. The
install message was not accepted as proof. Following both symlinks and checking
the versioned binary exposed the mismatch.

Later, an incident reported against 0.55.17 was actually running 0.55.16. That
fact changed which code paths could possibly be responsible.

## Read-only evidence ladder

Collect evidence from the least invasive layers first.

### 1. CLI state

```sh
sysg status --format json
sysg status -p <project> --format json
sysg inspect -p <project> -s <unit> --format json
sysg logs -p <project> -s <unit> --no-follow -l 200
sysg logs --supervisor --no-follow -l 400
```

Record the command, exit code, wall-clock time, selected project, and exact
binary version. A failing `status` exit code can be a health verdict, not an IPC
failure, so preserve its output.

### 2. Live process identity

Prefer PIDs returned by structured status:

```sh
pid='<pid-from-status>'
kill -0 "$pid"
ps -o pid=,ppid=,pgid=,state=,etime=,command= -p "$pid"
```

`kill -0` checks existence without sending a signal. Compare PID, parent PID,
process group, age, and command. A PID existing is not enough; it may be a
zombie, the wrong generation, or outside the supervisor's process group.

Avoid `pgrep -f '<command>'` as primary evidence. The shell executing `pgrep`
can match its own command line. If a broad process search is unavoidable,
cross-check every result against status and `ps` lineage.

### 3. Ports and external dependencies

```sh
lsof -nP -iTCP:<port> -sTCP:LISTEN
nc -z 127.0.0.1 <port>
```

A free port does not prove a supervisor failure, and an occupied port does not
identify its owner. Correlate the listener PID with the process table and
systemg's tracked PID.

### 4. Persisted state

User-mode project state lives under:

```text
~/.local/share/systemg/projects/<project>/pid.xml
~/.local/share/systemg/projects/<project>/state.xml
~/.local/share/systemg/projects/<project>/cron_state.xml
```

Read these files, but do not treat any one file as authoritative. Compare them
with the process table and current supervisor response.

### 5. Timestamps and logs

Use timestamps to reconstruct order:

```sh
stat ~/.local/share/systemg/logs/supervisor.log
tail -n 400 ~/.local/share/systemg/logs/supervisor.log
```

Log content alone can be historical. A current modification time alone can be
buffering or a reopened file. Require the timestamped content, process age, and
state transition to agree.

## Build a falsifiable timeline

Before reading code, write the incident as state transitions:

```text
T0 project registered
T1 pre_start began
T2 service process spawned
T3 health probes began
T4 dependency declared ready or failed
T5 dependent started or remained stopped
T6 foreground attachment continued or detached
```

For each step, record:

- expected event
- observed event
- evidence source
- earliest timestamp
- competing explanations

Then find the first transition where observed behavior diverges. Code after
that point is usually a consequence, not the cause.

## Examples from the 0.55 to 0.56 investigation

### Health timeout that lasted only 18 seconds

The arbitration manifest appeared to configure a 300-second health timeout,
but the start failed after roughly 18 seconds.

Evidence:

- supervisor timestamps showed health probing from 04:45:06 to 04:45:24
- exactly ten fast connection-refused attempts occurred
- the application compilation started after the probes had already begun
- the manifest used `health_check.timeout: 300s`
- the raw health-check parser had no `timeout` field and did not reject unknown
  keys

This disproved both "the database wait timed out at 300 seconds" and "the warm
service failed." The configured field was silently ignored. The fix introduced
`total_timeout`, accepted `timeout` as a compatibility alias, rejected unknown
health fields, and added a delayed-readiness regression where fast failures had
to continue beyond the retry floor.

### Warm-data logs looked healthy, but start reported it failed

The warm-data log ended successfully before the restart began. Timeline
comparison showed that the dependency service failed its readiness gate, so
warm-data was not launched in that generation. Its old healthy output was not
evidence that the new run had started.

This is why logs must be bounded by the incident timestamps and matched to a PID
generation.

### A cron row was `Failed` while the scheduler was working

Cron health describes the most recent completed run. `Failed` does not mean the
scheduler is down; it means the last run exited unsuccessfully. Inspect
`cron_state.xml`, the latest run record, and its completion timestamp. Confirm
that the next schedule remains registered before diagnosing a scheduler outage.

### PID state looked clean while status went stale

A concurrent restart bug was initially blamed on `pid.xml`. Repeated fixes
there did not help because the probe path had already reaped the process and
cleared the PID entry. It failed to persist the corresponding lifecycle update
to `state.xml`, and status later preferred the stale state record.

The decisive test compared both files and the OS after repeated concurrent
restarts. The first inconsistent transition was the missing state write, not
PID removal.

### Foreground stream EOF was mistaken for project death

A project-wide log follow spawned one tail per running unit and joined all tail
threads. When those tails ended, the server returned from the follow request.
The foreground client treated stream EOF as proof that the project was gone and
sent `StopProject`.

That local interpretation killed healthy projects. The correct invariant is:

```text
log stream ended != project ended
```

The client must verify supervisor and project state independently. A reconnect
fixture must include the structures that trigger real behavior: a completed
one-shot, dependency chain, slow pre-start, and long-running services.

### A project lookup blamed the wrong manifest

The command used `arbiration-dev`, while the loaded id was
`arbitration-dev`. The supervisor correctly listed both arbitration and
gamecast, but config resolution produced SG0201 because the requested id did
not match the resolved manifest.

Always copy stable ids from structured status. Do not infer them from the
working directory or display name.

## Terminal and signal correctness

`kill -INT <pid>` is not equivalent to typing Ctrl-C in a terminal. Foreground
behavior depends on the terminal's controlling process group and byte stream.

Use the PTY harness:

```sh
scripts/fgpty.py -- <command>
```

The harness sends byte `0x03` through the PTY master, which exercises the real
Ctrl-C path. Use it for raw-mode restoration, foreground ownership, log-stream
shutdown, and terminal repaint behavior.

When testing progress output, capture a real TTY and verify:

- elapsed seconds update on one row
- the row is cleared before the final message
- the final message begins after two newlines
- long text does not wrap into accumulating spinner rows
- terminal mode is restored on every exit path

## Concurrency invariants

Use these as review questions for every lifecycle change:

1. One resident supervisor owns the control socket.
2. One project id owns one runtime and one state directory.
3. A mutation either acquires the operation slot or returns SG0107.
4. Validation happens before teardown.
5. Unchanged services retain their PID during manifest reconciliation.
6. A targeted restart includes required dependents but not unrelated units.
7. Clean exit zero never triggers restart, regardless of `restart_policy`.
8. A stopped project does not stop sibling projects or the warm supervisor.
9. A stream disconnect does not imply process or project death.
10. CLI claims, persisted state, and the OS must agree after the operation.

## From live failure to regression case

Do not make the smallest possible fixture. Make the smallest fixture that
preserves the causal structure of the live stack.

For a foreground lifecycle bug, that may require:

- more than one project
- a completed one-shot
- a deep `depends_on` chain
- a slow `pre_start`
- a health check
- a cron unit
- proxy environment variables
- a second foreground attachment

Write the case after the live cause is understood. The case should fail for the
same reason, not merely produce the same final message.

## Verification ladder

Use progressively broader gates:

1. A focused unit test for the pure state decision.
2. A focused integration test for IPC or persisted state.
3. The exact Docker use case that reproduces the process topology.
4. All Rust tests.
5. All Docker use cases at a concurrency level the host can sustain.
6. `scripts/commit-check.sh` for formatting, Clippy, rustdoc, and docs links.
7. The real live workflow with the exact installed version.

A parallel timeout is evidence, not automatically a product failure. Rerun the
case alone several times, then rerun the full set at lower concurrency. In the
0.56 validation, one 15-second race case timed out under eight-way Docker load,
passed three isolated repetitions, and the full 111-case suite passed at
four-way concurrency.

## Definition of done

Do not move on until all applicable statements are true:

- the exact binary and source revision are known
- the failure reproduces or the original evidence explains why it cannot
- the first incorrect state transition is identified
- at least one competing hypothesis has been disproved
- the fix is limited to the causal boundary
- a structurally faithful regression case passes
- status, PID/process group, and persisted state agree
- no old process generation or orphan remains
- the user receives a specific SG code instead of SG0001 where possible
- the complete automated gates pass
- the live workflow succeeds on the installed artifact

## Incident report template

```text
Binary:
Revision:
Command and exit code:
Project/unit:
Expected transition:
First divergent transition:
Observed evidence:
Disproved hypotheses:
Root cause:
Fix boundary:
Focused regression:
Full gates:
Live verification:
Remaining uncertainty:
```

Never write "seems fixed" when one of these fields is unknown. State what was
observed, what is inferred, and what remains unverified.

## Mutation boundary

These operations change live state and require explicit intent:

```text
sysg start
sysg stop
sysg restart
sysg purge
kill / pkill
editing a live manifest or env file
opening or closing an application tunnel
```

If the request is read-only, none of them are debugging shortcuts. Gather the
evidence, explain the cause, and propose the mutation separately.
