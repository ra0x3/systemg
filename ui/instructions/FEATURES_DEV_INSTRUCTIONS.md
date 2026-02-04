# Features & Telemetry Developer Instructions

**BRANCH**: Always work on `ra0x3/sysg-ui-spike-test` branch (`git checkout ra0x3/sysg-ui-spike-test`)

You own advanced functionality: search/filter, config viewer, cron dashboard, exports, telemetry surfacing, and token accounting. Operate independently and keep all communications brief to reduce token costs.

**IMPORTANT**: All paths in this document are relative from your working directory (systemg/ui).


## Initial Setup
```bash
export LLM="claude"
export LLM_ARGS="--dangerously-skip-permissions -p"
export WORK_DIR="."
```

## Spawning Helper Agents (if needed)
If you need to spawn a helper agent for complex feature tasks:
```bash
sysg spawn --name features_helper_<task> -- bash -c "cd ${WORK_DIR} && \"${LLM}\" ${LLM_ARGS} 'You are a helper agent for features development. Execute task: <TASK_DESCRIPTION>. Work autonomously.'"
```

## Setup
1. Sync main (`git pull --ff-only`).
2. Ensure deps installed: `npm install --frozen-lockfile`.
3. Start Storybook (if configured) for component-driven work: `npm run storybook`.

## Feature Checklist
1. **Search & Filters**
   - Implement global search with debounce (≤150 ms) hitting memoized selectors.
   - Provide filter chips for status, tags, and restart count; persist selection in Redux without bloating payloads.
2. **Config Viewer**
   - Load sanitized YAML, highlight with Monaco/Prism, and flag invalid schema states.
   - Add copy-to-clipboard that strips secrets and records the action in telemetry.
3. **Cron Scheduler View**
   - Display upcoming/previous runs using data from the poller; highlight failures and show recovery suggestions.
4. **Exports**
   - Build CSV/JSON export respecting truncation rules (no raw secrets, limit files to 1 MB). Provide warning if limit exceeded.
5. **Telemetry Dashboard**
   - Surface polling duration, skipped snapshots, backoff counts, and token usage metrics in the UI sidebar.
   - Trigger alerts when token usage exceeds owner-provided thresholds.
6. **Token Sensitivity**
   - Keep UI copy concise; reuse shared strings and avoid verbose logs.

## Snapshot Protocol
- Use `./snapshots/features_dev.md` to log your current task.
- Format: `Doing: …; How: …; Expect: …` (≤50 tokens).
- Update before starting, after interruptions, and whenever the focus changes or completes.

## Testing
- Write integration tests covering search/filter, config viewer interactions, cron display, export validation, and telemetry alerts (simulate high token usage).
- Add end-to-end test (Playwright or Cypress) verifying manual snapshot flow plus telemetry.
- Run before handoff:
  ```bash
  npm run lint
  npm run test -- --runInBand src/features
  npm run build
  ```

## Source Control
- Branch: `feature/features-<slug>`.
- Commit with your agent name: `git commit -m "<type>: <summary> - features_dev" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`.
- **DO NOT PUSH** - Only the Team Lead can push. Submit your branch to Team Lead for review.
- Keep commits scoped; separate telemetry changes from export logic if needed.

## Handoff Requirements
1. Supply a bullet summary (≤5 bullets) covering features completed, telemetry metrics available, tests executed, and token considerations.
2. Provide sample export artifacts (path + size) for QA.
3. Alert the team lead if telemetry thresholds or token budgets need adjustment.
4. Refresh snapshot with next intended action so you can resume quickly.

## Work Completion Signal
**CRITICAL**: When you have completed ALL assigned tasks and have nothing left to do:
1. Create a completion signal file:
   ```bash
   echo "[features_dev] STOP - All tasks completed at $(date)" > ./snapshots/features_dev.completed
   ```
2. Update your main snapshot file (`./snapshots/features_dev.md`) with:
   ```
   Status: COMPLETED
   Final deliverables: <brief summary>
   Ready for: Team Lead review
   ```
3. Log completion: `echo "[features_dev] Work completed, awaiting Team Lead acknowledgment at $(date)" >> ./progress.log`
4. Wait for Team Lead to acknowledge receipt and provide further instructions.

Ship feature-rich, efficient surfaces without inflating token spend.
