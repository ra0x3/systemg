# Core Infrastructure Developer Instructions

**BRANCH**: Always work on `ra0x3/sysg-ui-spike-test` branch (`git checkout ra0x3/sysg-ui-spike-test`)

You own foundations: project scaffolding, poller, sanitisation, storage, and browser fallback. Work autonomously and keep every message short to conserve tokens.

**IMPORTANT**: All paths in this document are relative from your working directory (systemg/ui).

## Initial Setup
```bash
export LLM="claude"
export LLM_ARGS="--dangerously-skip-permissions -p"
export WORK_DIR="."
```

## Spawning Helper Agents (if needed)
If you need to spawn a helper agent for complex tasks:
```bash
sysg spawn --name core_helper_<task> -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'You are a helper agent for core infrastructure. Execute task: <TASK_DESCRIPTION>. Work autonomously.'"
```

## Project Bootstrap (CRITICAL - DO THIS FIRST)
1. **Create package.json** - The project does NOT exist yet. You must create it:
   ```bash
   npm init -y
   npm pkg set name="systemg-ui"
   npm pkg set version="1.0.0"
   npm pkg set type="module"
   npm pkg set scripts.dev="vite"
   npm pkg set scripts.build="tsc && vite build"
   npm pkg set scripts.preview="vite preview"
   npm pkg set scripts.test="vitest"
   npm pkg set scripts.lint="eslint src --ext ts,tsx"
   npm pkg set scripts.type-check="tsc --noEmit"
   ```

2. **Install dependencies**:
   ```bash
   npm install @reduxjs/toolkit react-redux @chakra-ui/react @emotion/react @emotion/styled framer-motion lucide-react react react-dom
   npm install -D vite @vitejs/plugin-react typescript @types/react @types/react-dom @types/node vitest jsdom @testing-library/react @testing-library/jest-dom eslint @typescript-eslint/eslint-plugin @typescript-eslint/parser prettier eslint-config-prettier
   ```

3. **Create project structure**:
   ```bash
   mkdir -p src/components src/hooks src/store src/utils tests
   ```

## Environment Setup
1. Create `.env.local` copying required secrets from `../.env` (for tests only).
2. Run `npm run lint` once to confirm the workspace is healthy (after creating eslint config).

## Implementation Checklist
1. **Redux Store**
   - Define slices that store only sanitized data (`sanitizedEnv`, truncated logs, capped metrics).
   - Ensure non-serializable objects (FileSystemDirectoryHandle) bypass serializable checks.
2. **Polling Toolkit**
   - Implement `readJsonSnapshot` (single-flight, `lastModified` cache, partial write tolerance, safe JSON parse).
   - Implement `readLogDelta` (per-file byte offsets, `Blob.slice`, 1 MiB cap, sanitization before returning).
   - Implement `toPollingMessage` mapping DOMException names to user-friendly copy.
   - Export helpers from `src/utils/files.ts` with exhaustive unit tests.
3. **useSystemGPoller Hook**
   - Integrate helpers, exponential backoff, and manual cleanup via `clearTimeout`.
   - Dispatch sanitized payloads only; never leak raw env/log data to Redux.
4. **Browser Compatibility + Manual Snapshot**
   - Implement feature detection using `checkFileAPISupport` from the spec.
   - Build a fallback flow prompting users to upload tarballs from `systemg export` when the File API is unavailable.
   - Persist log offsets/metrics history in IndexedDB for resume support.
5. **Telemetry**
   - Emit polling duration, backoff count, skipped snapshot counts, and token usage estimates to a shared telemetry store consumed by the features dev.

## Snapshot Protocol
- Before starting work or after any interruption, update `./snapshots/core_infra_dev.md` with a single sentence:
  `Doing: <task>; How: <approach>; Expect: <result>`.
- Keep the sentence under 50 tokens; update whenever your focus changes or you finish a deliverable.

## Testing Requirements
- Unit tests: cover `readJsonSnapshot`, `readLogDelta`, sanitisation utilities, error mapping, IndexedDB persistence.
- Integration smoke test: simulate partial write + permission errors using mocked FileSystem handles.
- Commands to run before handoff:
  ```bash
  npm run lint
  npm run type-check
  npm run test -- --runInBand src/utils/files.test.ts src/hooks/useSystemGPoller.test.ts
  npm run build
  ```
- Capture command output succinctly for the team lead.

## Source Control Rules
- Branch: `feature/core-infra-<slug>`.
- Commit with your agent name: `git commit -m "<type>: <summary> - core_infra_dev" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`.
- **DO NOT PUSH** - Only the Team Lead can push. Submit your branch to Team Lead for review.
- Keep commit summaries under 60 tokens.

## Handoff Procedure
1. Prepare a bullet summary (max 5 bullets) noting implemented helpers, tests run, and token impact.
2. Provide repro instructions for manual snapshot fallback and partial write handling.
3. Notify the team lead only when all checklist items pass.
4. Refresh the snapshot with next intended action (or "Idle awaiting review").

## Work Completion Signal
**CRITICAL**: When you have completed ALL assigned tasks and have nothing left to do:
1. Create a completion signal file:
   ```bash
   echo "[core_infra_dev] STOP - All tasks completed at $(date)" > ./snapshots/core_infra_dev.completed
   ```
2. Update your main snapshot file (`./snapshots/core_infra_dev.md`) with:
   ```
   Status: COMPLETED
   Final deliverables: <brief summary>
   Ready for: Team Lead review
   ```
3. Log completion: `echo "[core_infra_dev] Work completed, awaiting Team Lead acknowledgment at $(date)" >> ./progress.log`
4. Wait for Team Lead to acknowledge receipt and provide further instructions.

Stay focused, avoid verbosity, and deliver resilient infrastructure.