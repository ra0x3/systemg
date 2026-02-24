# Orchestrator Instructions

## ABSOLUTE REQUIREMENT: All Files Go In orchestrator-ui/ Folder

ALL files created by ANY team member MUST be inside the orchestrator-ui/ directory.
- NO files in parent directories
- NO files in sibling directories
- NO files in /reports or /docs or anywhere else
- EVERYTHING goes inside orchestrator-ui/

This is non-negotiable. Any files created outside orchestrator-ui/ will result in project rejection.

## CRITICAL: Build Order - Follow This Exactly

### The Problem We're Solving
Previous attempt: 1,926 lines of tests, 0 UI components, complete failure.
This happened because developers built tests and infrastructure for components that didn't exist.

### Mandatory Development Sequence

1. **UI FIRST**: Build React components that render with mock data
   - Success: `yarn dev` shows actual dashboard at localhost:5173
   - Not placeholder text, actual components

2. **Infrastructure SECOND**: Build services to support existing UI
   - Only build for components that already render
   - Replace mock data with real data

3. **State Management THIRD**: Connect services to UI via Redux
   - Only create slices for existing components
   - Wire up data flow

4. **Testing LAST**: Test only code that exists
   - No tests for imaginary components
   - Test files match component files

### Definition of Done
- Code runs (`yarn dev` works)
- UI displays in browser (not "Development environment ready")
- Users can interact with it

If you can't see it in the browser, it doesn't count as done.

## Runtime Prerequisites

- `sysg` (systemg CLI) must be installed and available on PATH.
- `porki` must be installed from PyPI and available on PATH.
- Redis must be running and reachable at `redis://127.0.0.1:6379`.

## Global Output Policy

All report outputs must be written under the `reports/` directory in this
orchestrator example.

## CRITICAL TASK ASSIGNMENT RULES FOR ORCHESTRATOR

### Role-to-Task Mapping (MANDATORY)

**ui-dev MUST handle ALL of these:**
- Initialize React/Vite Project
- Create ALL UI Components (Dashboard, ProcessList, LogViewer, ResourceMonitor, etc.)
- Build ALL Layout Components (Header, Sidebar, MainLayout)
- Build ALL Pages (Dashboard Page, Settings Page, etc.)
- Setup React Router
- Create App.tsx with actual components

**core-infra-dev handles ONLY:**
- File system access services (AFTER UI exists)
- Browser compatibility layer (AFTER UI exists)
- Polling mechanisms (AFTER UI exists)
- Data fetching from disk (AFTER UI exists)

**features-dev handles ONLY:**
- Redux store setup (AFTER UI components exist)
- State management slices (AFTER UI components exist)
- Mock data services (AFTER UI components need them)

**qa-dev handles ONLY:**
- Testing components that ALREADY EXIST
- Never writes tests for components that don't exist

### MANDATORY Task Ordering

The DAG MUST follow this sequence:
1. **PHASE 1 - UI Creation (tasks 1-10):** ui-dev builds ALL components with hardcoded mock data
2. **PHASE 2 - Infrastructure (tasks 11-15):** core-infra-dev replaces mock data with real data
3. **PHASE 3 - State Management (tasks 16-20):** features-dev connects components to Redux
4. **PHASE 4 - Testing (tasks 21+):** qa-dev tests existing components

### Task Assignment Violations to PREVENT

NEVER assign these to core-infra-dev:
- Create Layout Components
- Build Dashboard Page
- Build Log Viewer Component
- Any UI component creation

NEVER assign these to ui-dev:
- File system services
- Mock data services
- State management setup

NEVER create tasks for:
- Testing before components exist
- Infrastructure before UI exists
- State management before components exist

## Agent Configuration

```yaml
agents:
  - name: owner
    goal: orchestrator-ui
    heartbeat: heartbeat/OWNER.md
    instructions: OWNER.md
    log-level: INFO
    cadence: 30s

  - name: team-lead
    goal: orchestrator-ui
    heartbeat: heartbeat/TEAM_LEAD.md
    instructions: TEAM_LEAD.md
    log-level: INFO
    cadence: 30s

  - name: core-infra-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/CORE_INFRA_DEV.md
    instructions: CORE_INFRA_DEV.md
    log-level: INFO
    cadence: 30s

  - name: ui-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/UI_DEV.md
    instructions: UI_DEV.md
    log-level: INFO
    cadence: 30s

  - name: features-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/FEATURES_DEV.md
    instructions: FEATURES_DEV.md
    log-level: INFO
    cadence: 30s

  - name: qa-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/QA_DEV.md
    instructions: QA_DEV.md
    log-level: INFO
    cadence: 30s
```
