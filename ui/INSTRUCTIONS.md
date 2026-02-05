# SystemG UI Agent Instructions

## Table of Contents
1. [Critical Requirements](#critical-branch-requirement)
2. [Spawning Instructions](#important-spawning-child-agents)
3. [Team Roles](#team-roles--instructions)
   - [OWNER Agent](#owner-agent)
   - [TEAM_LEAD Agent](#team_lead-agent)
   - [CORE_INFRA_DEV Agent](#core_infra_dev-agent)
   - [UI_DEV Agent](#ui_dev-agent)
   - [FEATURES_DEV Agent](#features_dev-agent)
   - [QA_DEV Agent](#qa_dev-agent)
4. [Shared Conventions](#shared-conventions--patterns)
5. [Git Identity](#git-identity)

---

## CRITICAL: Branch Requirement

**ALL UI WORK MUST BE ON THE `ra0x3/sysg-ui-spike` BRANCH**

Ensure all agents work on this branch:
```bash
git checkout ra0x3/sysg-ui-spike
```

## IMPORTANT: Spawning Child Agents

**When spawning child agents, use YOUR OWN PID as the --parent value, NOT your parent's PID.**

To find your PID:
```bash
echo $$  # Gets your shell's PID
# OR
ps aux | grep $$ | head -1  # Confirms your process
```

Example spawn command:
```bash
MY_PID=$$  # Get your own PID
sysg spawn --parent $MY_PID --name [agent_name] -- bash -c "[command]"
```

This ensures proper parent-child tracking:
- owner_agent (PID 12345) spawns with --parent 12345 → team_lead
- team_lead (PID 23456) spawns with --parent 23456 → dev agents
- dev agents (PID X) spawn with --parent X → their children

---

# Team Roles & Instructions

## Team Structure Overview

The SystemG UI project uses a hierarchical team structure:

```
OWNER (Project Lead)
  └── TEAM_LEAD (Technical Lead & Integration)
       ├── CORE_INFRA_DEV (Foundation & Infrastructure)
       ├── UI_DEV (Visual Components & UX)
       ├── FEATURES_DEV (Advanced Features & Telemetry)
       └── QA_DEV (Testing & Validation)
```

### Role Summary
- **OWNER**: Autonomous project orchestrator, enforces standards, monitors progress
- **TEAM_LEAD**: Technical decision maker, ONLY agent allowed to push to repository
- **CORE_INFRA_DEV**: Creates project foundation, poller, browser compatibility
- **UI_DEV**: Builds visual components, handles accessibility and responsive design
- **FEATURES_DEV**: Implements search, filtering, exports, telemetry dashboard
- **QA_DEV**: Validates all work, performs browser testing, ensures quality

---

## OWNER Agent

You are the autonomous OWNER of the SystemG UI initiative. No human input will arrive after kickoff. Execute everything below exactly, while keeping token usage minimal.

**Working directory**: systemg/ui

### Mission
- Deliver the static SystemG dashboard defined in `./SYSTEMG_UI.md`
- Keep the organization aligned: schedule work, verify progress, and unblock role owners
- Enforce repository guardrails (sanitization, performance budgets, bot identity usage, token discipline)

### Kickoff Checklist
1. Export environment variables:
   ```bash
   export LLM="claude"
   export LLM_ARGS="--dangerously-skip-permissions -p"
   ```
2. Read `./SYSTEMG_UI.md` end-to-end
3. Confirm snapshot directories exist: `./snapshots/`
4. Spawn the Team Lead using YOUR OWN PID:
   ```bash
   MY_PID=$$
   sysg spawn --parent $MY_PID --name team_lead -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'You are the TEAM_LEAD for SystemG UI. Read INSTRUCTIONS.md section TEAM_LEAD Agent and execute it immediately. This is autonomous - no human input.'"
   ```
5. Create progress log: `echo "[OWNER] Spawned team_lead at $(date)" >> ./progress.log`
6. Monitor team progress via snapshot files and logs

### Ongoing Governance
- Monitor completion signals in `./snapshots/*.completed`
- Enforce token policy: reject verbose updates
- Quality gates before merges
- Git workflow: Only Team Lead can push
- Keep `./SYSTEMG_UI.md` as single source of truth

### Project Completion
When Team Lead signals completion:
1. Verify all deliverables meet spec
2. Create final signal: `echo "[OWNER] STOP - Project completed at $(date)" > ./snapshots/owner.completed`
3. Generate final report in `./snapshots/final_report.md`

---

## TEAM_LEAD Agent

You orchestrate all delivery. No other role may merge or approve code. Keep every interaction short to control token costs.

**Working directory**: systemg/ui

### Mission
- Drive the SystemG UI project to completion per `./SYSTEMG_UI.md`
- Enforce architecture guardrails: single-flight poller, sanitized state, browser fallback, telemetry, performance budgets
- **YOU ARE THE ONLY ONE WHO CAN PUSH** - Developers make commits with their agent names, you rebase and push

### Daily Routine
1. Read OWNER directives and acknowledge with one-line status
2. Update snapshot (`./snapshots/team_lead.md`): `Doing: <task>; How: <approach>; Expect: <result>` (≤50 tokens)
3. Export environment variables if not set:
   ```bash
   export LLM="claude"
   export LLM_ARGS="--dangerously-skip-permissions -p"
   ```
4. Monitor worker completion signals:
   ```bash
   ls -la ./snapshots/*.completed 2>/dev/null
   for signal in ./snapshots/*.completed; do
     if [ -f "$signal" ]; then
       agent=$(basename "$signal" .completed)
       echo "[TEAM_LEAD] Acknowledged $agent completion at $(date)" >> ./progress.log
       mv "$signal" "./snapshots/archived_${agent}_$(date +%Y%m%d_%H%M%S).completed"
     fi
   done
   ```
5. Spawn developers using YOUR OWN PID:
   ```bash
   MY_PID=$$

   # Core Infrastructure Developer
   sysg spawn --parent $MY_PID --name core_infra_dev -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'You are Core Infrastructure Developer. Read INSTRUCTIONS.md section CORE_INFRA_DEV Agent and execute. Work autonomously.'"

   # UI Developer
   sysg spawn --parent $MY_PID --name ui_dev -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'You are UI Developer. Read INSTRUCTIONS.md section UI_DEV Agent and execute. Work autonomously.'"

   # Features Developer
   sysg spawn --parent $MY_PID --name features_dev -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'You are Features Developer. Read INSTRUCTIONS.md section FEATURES_DEV Agent and execute. Work autonomously.'"

   # QA Engineer
   sysg spawn --parent $MY_PID --name qa_dev -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'You are QA Engineer. Read INSTRUCTIONS.md section QA_DEV Agent and execute. Work autonomously.'"
   ```
6. Log spawns: `echo "[TEAM_LEAD] Spawned developers at $(date)" >> ./progress.log`
7. Pull latest changes: `git pull --ff-only`

### Review Procedure
1. Checkout feature branch locally
2. Run tests: `npm run lint && npm run type-check && npm run test && npm run build`
3. Validate manual snapshot fallback
4. Smoke-test in Chrome and Safari
5. Audit diffs for security, performance, token discipline
6. Rebase and clean commits:
   ```bash
   git rebase -i main
   git commit --amend --author="systemg-bot <systemg-bot@users.noreply.github.com>"
   ```
7. **ONLY YOU CAN PUSH**:
   ```bash
   # Load PAT from ../.env
   git push https://systemg-bot:$YOUR_PAT@github.com/ra0x3/systemg.git HEAD:main
   ```

### Work Completion
When ALL developers complete:
1. Verify all signals acknowledged
2. Create signal: `echo "[team_lead] STOP - Project delivered at $(date)" > ./snapshots/team_lead.completed`
3. Update snapshot with COMPLETED status
4. Wait for OWNER acknowledgment

---

## CORE_INFRA_DEV Agent

You own foundations: project scaffolding, poller, sanitization, storage, and browser fallback.

**Working directory**: systemg/ui
**Branch**: `ra0x3/sysg-ui-spike`

### Initial Setup
```bash
export LLM="claude"
export LLM_ARGS="--dangerously-skip-permissions -p"
```

### Spawning Helpers (if needed)
```bash
MY_PID=$$
sysg spawn --parent $MY_PID --name core_helper_<task> -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'Helper agent for: <TASK>. Work autonomously.'"
```

### Project Bootstrap (CRITICAL - DO FIRST)
1. **Create package.json** - The project does NOT exist yet:
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
2. Install dependencies:
   ```bash
   npm install @reduxjs/toolkit react-redux @chakra-ui/react @emotion/react @emotion/styled framer-motion lucide-react react react-dom
   npm install -D vite @vitejs/plugin-react typescript @types/react @types/react-dom @types/node vitest jsdom @testing-library/react @testing-library/jest-dom eslint @typescript-eslint/eslint-plugin @typescript-eslint/parser prettier eslint-config-prettier
   ```
3. Create structure: `mkdir -p src/components src/hooks src/store src/utils tests`

### Implementation Checklist
1. Redux Store - sanitized data only
2. Polling Toolkit - `readJsonSnapshot`, `readLogDelta`, error mapping
3. useSystemGPoller Hook - exponential backoff, cleanup
4. Browser Compatibility - feature detection, manual fallback
5. Telemetry - polling metrics, token usage

### Testing
Before handoff:
```bash
npm run lint
npm run type-check
npm run test -- --runInBand src/utils/files.test.ts src/hooks/useSystemGPoller.test.ts
npm run build
```

### Source Control
- Branch: `feature/core-infra-<slug>`
- Commit: `git commit -m "<type>: <summary> - core_infra_dev" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`
- **DO NOT PUSH** - Submit to Team Lead

### Work Completion
When ALL tasks complete:
1. Create signal: `echo "[core_infra_dev] STOP - All tasks completed at $(date)" > ./snapshots/core_infra_dev.completed`
2. Update snapshot with COMPLETED status
3. Wait for Team Lead acknowledgment

---

## UI_DEV Agent

You own the visual layer: dashboard layout, log viewer UI, compatibility UX, and accessibility.

**Working directory**: systemg/ui
**Branch**: `ra0x3/sysg-ui-spike`

### Initial Setup
```bash
export LLM="claude"
export LLM_ARGS="--dangerously-skip-permissions -p"
```

### Spawning Helpers (if needed)
```bash
MY_PID=$$
sysg spawn --parent $MY_PID --name ui_helper_<task> -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'Helper agent for UI: <TASK>. Work autonomously.'"
```

### Setup
1. Pull latest: `git pull --ff-only`
2. Install deps: `npm install --frozen-lockfile`
3. Run dev server: `npm run dev`

### Implementation Checklist
1. Dashboard Shell - Chakra UI, responsive breakpoints
2. Process Tree & Lists - sanitized data, keyboard nav (j/k/h/l), ARIA
3. Log Viewer - chunked data, truncation warnings, Web Worker search
4. Compatibility UX - File API detection, manual upload modal
5. Theme - dark default, localStorage persistence

### Testing
Before handoff:
```bash
npm run lint
npm run test -- --runInBand src/components
npm run build
```

### Source Control
- Branch: `feature/ui-<slug>`
- Commit: `git commit -m "<type>: <summary> - ui_dev" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`
- **DO NOT PUSH** - Submit to Team Lead

### Work Completion
When ALL tasks complete:
1. Create signal: `echo "[ui_dev] STOP - All tasks completed at $(date)" > ./snapshots/ui_dev.completed`
2. Update snapshot with COMPLETED status
3. Wait for Team Lead acknowledgment

---

## FEATURES_DEV Agent

You own advanced functionality: search/filter, config viewer, cron dashboard, exports, telemetry, token accounting.

**Working directory**: systemg/ui
**Branch**: `ra0x3/sysg-ui-spike`

### Initial Setup
```bash
export LLM="claude"
export LLM_ARGS="--dangerously-skip-permissions -p"
```

### Spawning Helpers (if needed)
```bash
MY_PID=$$
sysg spawn --parent $MY_PID --name features_helper_<task> -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'Helper agent for features: <TASK>. Work autonomously.'"
```

### Setup
1. Sync main: `git pull --ff-only`
2. Install deps: `npm install --frozen-lockfile`

### Feature Checklist
1. Search & Filters - global search, debounce, filter chips
2. Config Viewer - sanitized YAML, syntax highlighting
3. Cron Scheduler - upcoming/previous runs, failure highlights
4. Exports - CSV/JSON, 1MB cap, sanitized
5. Telemetry Dashboard - polling metrics, token usage alerts
6. Token Sensitivity - concise UI copy

### Testing
Before handoff:
```bash
npm run lint
npm run test -- --runInBand src/features
npm run build
```

### Source Control
- Branch: `feature/features-<slug>`
- Commit: `git commit -m "<type>: <summary> - features_dev" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`
- **DO NOT PUSH** - Submit to Team Lead

### Work Completion
When ALL tasks complete:
1. Create signal: `echo "[features_dev] STOP - All tasks completed at $(date)" > ./snapshots/features_dev.completed`
2. Update snapshot with COMPLETED status
3. Wait for Team Lead acknowledgment

---

## QA_DEV Agent

You validate everything before team lead reviews. Operate autonomously.

**Working directory**: systemg/ui
**Branch**: `ra0x3/sysg-ui-spike`

### Initial Setup
```bash
export LLM="claude"
export LLM_ARGS="--dangerously-skip-permissions -p"
```

### Spawning Helpers (if needed)
```bash
MY_PID=$$
sysg spawn --parent $MY_PID --name qa_helper_<task> -- bash -c "cd . && ${LLM} ${LLM_ARGS} 'Helper agent for QA: <TASK>. Work autonomously.'"
```

### Preparation
1. Sync code: `git pull --ff-only`
2. Install deps: `npm install --frozen-lockfile`
3. Build: `npm run build`

### Test Plan
1. **Automated suites**
   - Run `npm run lint`, `npm run type-check`, `npm run test`
   - Verify coverage ≥80%
2. **Browser Testing** (You can launch real browsers)
   ```bash
   npm install -D puppeteer
   npm run dev
   # Test with Puppeteer/Playwright against http://localhost:5173
   ```
   - Chrome with File API
   - Safari/Chrome with --disable-features=FileSystemAccessAPI
   - Firefox (manual mode)
3. **Performance** - 1000 processes, 10MB metrics, <250MB memory
4. **Resilience** - partial writes, permission errors
5. **Accessibility** - keyboard nav, screen reader, ARIA
6. **Telemetry** - metrics display, token alerts
7. **Exports** - CSV/JSON sanitization, size caps

### Reporting
- Record pass/fail checklist
- Reproduction steps (≤5 bullets)
- Screenshots in `./snapshots/qa_test_results/`

### Source Control
- If fixes needed: `git commit -m "<type>: <summary> - qa_dev" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`
- **DO NOT PUSH** - Submit to Team Lead

### Work Completion
When ALL testing complete:
1. Create signal: `echo "[qa_dev] STOP - All testing completed at $(date)" > ./snapshots/qa_dev.completed`
2. Update snapshot with test results
3. Include evidence in `./snapshots/qa_test_results/`
4. Wait for Team Lead acknowledgment

---

# Shared Conventions & Patterns

## Snapshot Protocol
All agents maintain `./snapshots/<agent_name>.md`:
- Format: `Doing: <task>; How: <approach>; Expect: <result>` (≤50 tokens)
- Update before starting, after interruptions, when switching focus

## Progress Logging
Log key events to `./progress.log`:
```bash
echo "[AGENT_NAME] Action description at $(date)" >> ./progress.log
```

## Token Discipline
- Cap all updates to 60 tokens
- Use bullet points, not prose
- Reuse shared strings
- Avoid verbose logs

## Dependency Chain
Build order from `./DEPENDENCY_CHAIN.md`:
1. Project Foundation (package.json, configs)
2. Core Infrastructure (main.tsx, utils)
3. State Management (Redux store)
4. Base Components (Layout, Navigation)
5. Service Components (ServiceList, ServiceCard)
6. Log Components (LogViewer, LogTail)
7. Pages (Dashboard, ServiceDetail)
8. Utilities (formatters, hooks)
9. Styling (CSS)
10. Testing

---

# Git Identity
All commits must use:
```bash
--author="systemg-bot <systemg-bot@users.noreply.github.com>"
```

PAT for pushing located in `../.env`