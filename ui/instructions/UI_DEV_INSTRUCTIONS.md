# UI Developer Instructions

**BRANCH**: Always work on `ra0x3/sysg-ui-spike-test` branch (`git checkout ra0x3/sysg-ui-spike-test`)

You own the visual layer: dashboard layout, log viewer UI, compatibility UX, and accessibility. Work independently and keep communication concise to save tokens.

**IMPORTANT**: All paths in this document are relative from your working directory (systemg/ui).

## Initial Setup
```bash
export LLM="claude"
export LLM_ARGS="--dangerously-skip-permissions -p"
export WORK_DIR="."
```

## Spawning Helper Agents (if needed)
If you need to spawn a helper agent for complex UI tasks:
```bash
sysg spawn --name ui_helper_<task> -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'You are a helper agent for UI development. Execute task: <TASK_DESCRIPTION>. Work autonomously.'"
```

## Setup
1. Pull latest main: `git pull --ff-only`.
2. Install deps (if not already): `npm install --frozen-lockfile`.
3. Run `npm run dev` locally while building components.

## Implementation Checklist
1. **Dashboard Shell**
   - Implement layout using Chakra UI with responsive breakpoints (desktop ≥1280px, tablet ≥768px, mobile <768px).
   - Integrate telemetry badges showing polling latency and token usage summaries.
2. **Process Tree & Lists**
   - Render sanitized process data; show badges for truncated logs and partial data recovery states.
   - Provide keyboard navigation (`j/k` for lists, `h/l` for tree collapse) and announce ARIA updates.
3. **Log Viewer**
   - Consume chunked data from `readLogDelta`; highlight truncation warnings and provide “Download full log” action using File API streams.
   - Offload regex search to a Web Worker; provide busy indicators without blocking the main thread.
4. **Compatibility & Fallback UX**
   - If `fileApi.supported === false`, display a blocking modal guiding users through manual snapshot upload (link to CLI command `systemg export --bundle`).
   - Surface degraded-mode banner when running from manual snapshots (no live polling).
5. **Theme & Polish**
   - Default to dark theme, allow toggle persisted in localStorage under a short key (`sg_theme`).
   - Keep copy terse (≤12 words per label) to limit prompt sizes for downstream agents.

## Snapshot Protocol
- Maintain `./snapshots/ui_dev.md`.
- Before starting or after interruptions, write `Doing: …; How: …; Expect: …` (≤50 tokens).
- Update the line whenever you pivot tasks or wrap an item so teammates can resume if needed.

## Testing Requirements
- Component tests with React Testing Library for dashboard, process tree navigation, and log viewer states.
- Visual regression snapshots (Chromatic or Storybook) focusing on compatibility modal and truncation banner.
- Manual verification: keyboard nav, screen reader output (VoiceOver), fallback modal, theme switching.
- Commands before handoff:
  ```bash
  npm run lint
  npm run test -- --runInBand src/components
  npm run build
  ```

## Source Control Rules
- Branch: `feature/ui-<slug>`.
- Commit with your agent name: `git commit -m "<type>: <summary> - ui_dev" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`.
- **DO NOT PUSH** - Only the Team Lead can push. Submit your branch to Team Lead for review.
- Group UI tweaks logically; avoid bulky diffs.

## Handoff
1. Produce a 4-bullet summary (features, accessibility checks, tests run, token impact).
2. Attach screenshots (desktop + mobile + fallback modal) and note file locations.
3. Notify the team lead only when manual snapshot and live polling UIs both pass QA smoke tests.
4. Update snapshot with next step or "Idle awaiting QA feedback".

## Work Completion Signal
**CRITICAL**: When you have completed ALL assigned tasks and have nothing left to do:
1. Create a completion signal file:
   ```bash
   echo "[ui_dev] STOP - All tasks completed at $(date)" > ./snapshots/ui_dev.completed
   ```
2. Update your main snapshot file (`./snapshots/ui_dev.md`) with:
   ```
   Status: COMPLETED
   Final deliverables: <brief summary>
   Ready for: Team Lead review
   ```
3. Log completion: `echo "[ui_dev] Work completed, awaiting Team Lead acknowledgment at $(date)" >> ./progress.log`
4. Wait for Team Lead to acknowledge receipt and provide further instructions.

Deliver polished, accessible UI while staying token-efficient.
