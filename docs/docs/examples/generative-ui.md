---
sidebar_position: 50
title: Generative UI
---

# Generative UI

A fully autonomous AI-driven development team that builds a complete SystemG dashboard UI from specifications, demonstrating advanced multi-agent orchestration with proper parent-child process tracking.

## Overview

This example showcases how `systemg` can orchestrate a team of autonomous Claude agents to build a complete web application. The agents work hierarchically, with each role having specific responsibilities, from project management to implementation and quality assurance.

## Architecture

The project uses a hierarchical team structure with proper parent-child spawning:

```
OWNER (Project Lead)
  └── TEAM_LEAD (Technical Lead & Integration)
       ├── CORE_INFRA_DEV (Foundation & Infrastructure)
       ├── UI_DEV (Visual Components & UX)
       ├── FEATURES_DEV (Advanced Features & Telemetry)
       └── QA_DEV (Testing & Validation)
```

### Key Innovation: Proper Parent-Child Tracking

Each agent spawns children using its **own PID** as the parent, not its parent's PID:

```bash
# Correct spawning pattern
MY_PID=$$  # Get agent's own PID
sysg spawn --parent-pid $MY_PID --name child_agent -- command
```

This ensures proper process tree tracking:
- owner_agent (PID 12345) spawns with `--parent-pid 12345` → team_lead
- team_lead (PID 23456) spawns with `--parent-pid 23456` → dev agents
- dev agents (PID X) spawn with `--parent-pid X` → their helpers

## Features Demonstrated

### 1. Dynamic Agent Spawning
- Hierarchical team structure with depth and descendant limits
- Cascade termination policy for clean shutdown
- Dynamic spawning based on project needs

### 2. Autonomous Development Workflow
- **OWNER**: Project orchestration, progress monitoring, quality gates
- **TEAM_LEAD**: Technical decisions, code review, git operations (only agent allowed to push)
- **CORE_INFRA_DEV**: Project scaffolding, polling infrastructure, browser compatibility
- **UI_DEV**: Visual components, accessibility, responsive design
- **FEATURES_DEV**: Search/filtering, exports, telemetry dashboard
- **QA_DEV**: Browser testing, performance validation, quality assurance

### 3. File-Based Communication
- Progress tracking via `./snapshots/*.md` files
- Completion signals with `*.completed` files
- Shared progress log for audit trail
- Git workflow with systemg-bot identity

### 4. Token Efficiency
- Concise communication protocol (≤50 tokens per update)
- Structured snapshot format: `Doing: <task>; How: <approach>; Expect: <result>`
- Rejection of verbose updates to control costs

## Project Structure

```
examples/gen-ui/
├── systemg.yaml           # SystemG configuration
├── INSTRUCTIONS.md        # Consolidated team instructions
├── SYSTEMG_UI.md         # UI requirements specification
├── snapshots/            # Progress tracking directory
├── cleanup.sh            # Environment cleanup script
└── .env.example          # Environment configuration template
```

## Configuration

The `systemg.yaml` file configures the OWNER agent with spawn limits:

```yaml
version: "1"

services:
  owner_agent:
    command: |
      bash -c 'claude --dangerously-skip-permissions -p "..."'
    spawn:
      mode: "dynamic"
      limits:
        children: 6        # Owner can spawn Team Lead + developers
        depth: 4          # Four levels of hierarchy
        descendants: 30   # Total agents across all levels
        termination_policy: "cascade"
    env:
      vars:
        LLM: "claude"
        LLM_ARGS: '--dangerously-skip-permissions -p'
        AGENT_NAME: "owner_agent"
        WORK_DIR: "."
```

## Key Concepts

### 1. Completion Signals
Agents signal task completion using dedicated files:

```bash
echo "[agent_name] STOP - All tasks completed at $(date)" > ./snapshots/agent_name.completed
```

The supervising agent monitors and acknowledges these signals:

```bash
for signal in ./snapshots/*.completed; do
  if [ -f "$signal" ]; then
    agent=$(basename "$signal" .completed)
    echo "[SUPERVISOR] Acknowledged $agent completion" >> ./progress.log
    mv "$signal" "./snapshots/archived_${agent}_$(date +%Y%m%d_%H%M%S).completed"
  fi
done
```

### 2. Git Workflow
Only the TEAM_LEAD can push to the repository:

```bash
# Developer commits locally
git commit -m "feat: add feature - ui_dev" --author="systemg-bot <systemg-bot@users.noreply.github.com>"

# Team Lead rebases and pushes
git rebase -i main
git push https://systemg-bot:$PAT@github.com/org/repo.git HEAD:main
```

### 3. Browser Testing
QA agents can launch real browsers for testing:

```bash
npm install -D puppeteer
npm run dev

# Automated browser testing
node -e "
const puppeteer = require('puppeteer');
(async () => {
  const browser = await puppeteer.launch({ headless: false });
  const page = await browser.newPage();
  await page.goto('http://localhost:5173');
  await page.screenshot({ path: './snapshots/qa_screenshot.png' });
  await browser.close();
})();
"
```

## Running the Example

1. **Setup environment:**
   ```bash
   cd examples/gen-ui
   cp .env.example .env
   # Add your GitHub PAT token to .env
   ```

2. **Start the autonomous development:**
   ```bash
   sysg start
   ```

3. **Monitor progress:**
   ```bash
   # Watch overall status
   sysg status

   # Monitor specific agent
   sysg inspect owner_agent

   # View logs
   sysg logs owner_agent -f

   # Check progress snapshots
   ls -la snapshots/
   cat snapshots/team_lead.md
   ```

4. **View spawned agents:**
   ```bash
   sysg status --tree
   ```

## Observability

Monitor the autonomous development process through multiple channels:

### 1. SystemG Commands
- `sysg status`: View all agent states and parent-child relationships
- `sysg inspect <agent>`: Detailed view of specific agent
- `sysg logs <agent> -f`: Follow agent output in real-time
- `sysg status --tree`: Visualize the process hierarchy

### 2. Progress Tracking
- `./progress.log`: Chronological event log
- `./snapshots/*.md`: Current status of each agent
- `./snapshots/*.completed`: Completion signals

### 3. Git History
Track development progress through git commits:
```bash
git log --oneline --graph
```

## Cleanup

To reset the environment for a fresh run:

```bash
./cleanup.sh
```

This preserves:
- INSTRUCTIONS.md
- SYSTEMG_UI.md
- .env files
- Source code
- package.json

## Key Takeaways

1. **Proper Parent-Child Tracking**: Agents use their own PID when spawning children, maintaining accurate process hierarchies

2. **Autonomous Coordination**: Agents work independently while coordinating through file-based signals and snapshots

3. **Token Efficiency**: Structured communication protocols minimize LLM token usage

4. **Quality Gates**: Only the Team Lead can push code, ensuring review and consolidation

5. **Observable Progress**: Multiple monitoring channels provide visibility into the autonomous development process

## Further Reading

- [SystemG Configuration Reference](/docs/configuration)
- [Spawn Configuration](/docs/configuration#spawn-configuration)
- [Meta-Agents Example](/docs/examples/meta-agents)
- [State Management](/docs/state)
