# Task Assignment Rules for Orchestrator

## CRITICAL: Correct Role Assignment is Mandatory

The previous project failed because tasks were assigned to the wrong agents. This document defines exactly which agent handles which tasks.

## Task Categories and Ownership

### UI Development Tasks (ui-dev ONLY)

These tasks MUST be assigned to `ui-dev`:

```yaml
ui_dev_tasks:
  - title: "Initialize React/Vite Project"
    artifacts: ["orchestrator-ui/package.json", "orchestrator-ui/vite.config.ts"]

  - title: "Create App Component"
    artifacts: ["orchestrator-ui/src/App.tsx"]

  - title: "Build Dashboard Component"
    artifacts: ["orchestrator-ui/src/components/Dashboard.tsx"]

  - title: "Build Process List Component"
    artifacts: ["orchestrator-ui/src/components/ProcessList.tsx"]

  - title: "Build Log Viewer Component"
    artifacts: ["orchestrator-ui/src/components/LogViewer.tsx"]

  - title: "Build Resource Monitor Component"
    artifacts: ["orchestrator-ui/src/components/ResourceMonitor.tsx"]

  - title: "Create Layout Components"
    artifacts: ["orchestrator-ui/src/components/Layout/Header.tsx", "orchestrator-ui/src/components/Layout/Sidebar.tsx"]

  - title: "Build Metrics Display Component"
    artifacts: ["orchestrator-ui/src/components/MetricsDisplay.tsx"]

  - title: "Build Status Bar Component"
    artifacts: ["orchestrator-ui/src/components/StatusBar.tsx"]

  - title: "Setup React Router"
    artifacts: ["orchestrator-ui/src/router/index.tsx"]
```

### Infrastructure Tasks (core-infra-dev ONLY)

These tasks MUST be assigned to `core-infra-dev` but ONLY AFTER UI components exist:

```yaml
core_infra_tasks:
  - title: "Create File System Service"
    prerequisite: "UI components must exist first"
    artifacts: ["orchestrator-ui/src/services/fileSystem.ts"]

  - title: "Build Polling Service"
    prerequisite: "Dashboard component must exist"
    artifacts: ["orchestrator-ui/src/services/poller.ts"]

  - title: "Implement Browser Compatibility Layer"
    prerequisite: "UI components must exist"
    artifacts: ["orchestrator-ui/src/services/browserCompat.ts"]
```

### State Management Tasks (features-dev ONLY)

These tasks MUST be assigned to `features-dev` but ONLY AFTER UI components exist:

```yaml
features_dev_tasks:
  - title: "Setup Redux Store"
    prerequisite: "UI components must exist first"
    artifacts: ["orchestrator-ui/src/store/index.ts"]

  - title: "Create Process Slice"
    prerequisite: "ProcessList component must exist"
    artifacts: ["orchestrator-ui/src/store/processSlice.ts"]

  - title: "Create Mock Data Service"
    prerequisite: "Components needing data must exist"
    artifacts: ["orchestrator-ui/src/services/mockData.ts"]
```

### Testing Tasks (qa-dev ONLY)

These tasks MUST be assigned to `qa-dev` but ONLY AFTER components exist:

```yaml
qa_dev_tasks:
  - title: "Test Dashboard Component"
    prerequisite: "Dashboard.tsx must exist"
    artifacts: ["orchestrator-ui/src/components/Dashboard.test.tsx"]

  - title: "Test Process List Component"
    prerequisite: "ProcessList.tsx must exist"
    artifacts: ["orchestrator-ui/src/components/ProcessList.test.tsx"]
```

## Task Ordering Rules

### Phase 1: UI Components (Tasks 1-10)
All handled by `ui-dev`:
1. Initialize React/Vite project
2. Create App.tsx shell
3. Build Dashboard component with mock data
4. Build ProcessList component with mock data
5. Build LogViewer component with mock data
6. Build ResourceMonitor component with mock data
7. Create Layout components (Header, Sidebar)
8. Build MetricsDisplay component
9. Build StatusBar component
10. Setup React Router

### Phase 2: Infrastructure (Tasks 11-15)
All handled by `core-infra-dev`:
11. Create File System service
12. Build Polling service
13. Implement Browser Compatibility
14. Create Log Reader service
15. Build Data Sanitizer

### Phase 3: State Management (Tasks 16-20)
All handled by `features-dev`:
16. Setup Redux store
17. Create process slice
18. Create metrics slice
19. Create logs slice
20. Wire up components to Redux

### Phase 4: Testing (Tasks 21+)
All handled by `qa-dev`:
21. Test existing components
22. Integration testing
23. Performance testing
24. E2E testing

## Common Mistakes to Avoid

### WRONG Assignments (caused previous failure):
- ❌ core-infra-dev: "Create Layout Components"
- ❌ core-infra-dev: "Build Dashboard Page"
- ❌ core-infra-dev: "Build Log Viewer Component"
- ❌ features-dev: "Build Resource Monitor Component"
- ❌ qa-dev: "Create test suite" (before components exist)

### CORRECT Assignments:
- ✓ ui-dev: "Create Layout Components"
- ✓ ui-dev: "Build Dashboard Page"
- ✓ ui-dev: "Build Log Viewer Component"
- ✓ ui-dev: "Build Resource Monitor Component"
- ✓ qa-dev: "Test Dashboard Component" (after it exists)

## Validation Checklist

Before creating a task, verify:
1. Is it a UI component? → Assign to ui-dev
2. Is it file/data access? → Assign to core-infra-dev (after UI exists)
3. Is it Redux/state? → Assign to features-dev (after UI exists)
4. Is it testing? → Assign to qa-dev (after component exists)

## Agent Rejection Rules

Each agent should REJECT tasks that don't belong to them:

### ui-dev rejects:
- File system services
- Redux/state management
- Testing tasks
- Infrastructure tasks

### core-infra-dev rejects:
- ANY UI component creation
- Redux/state management
- Testing tasks
- Layout components

### features-dev rejects:
- UI component creation
- File system services
- Testing tasks
- Infrastructure tasks

### qa-dev rejects:
- ANY implementation tasks
- Tasks for non-existent components