# SystemG UI Root Instructions

## Important: Branch Requirement

**ALL UI DEVELOPMENT WORK MUST BE DONE ON THE `ra0x3/sysg-ui-spike-test` BRANCH**

Before starting any work:
```bash
git checkout ra0x3/sysg-ui-spike-test
```

All feature branches must be created from `ra0x3/sysg-ui-spike-test` and all PRs must target this branch.

---

You are tasked with building a complete SystemG UI application. This is a React + TypeScript + Vite application that provides a static web dashboard for monitoring SystemG services.

## Your Goal

Create a fully functional UI for SystemG that allows users to:
1. View all registered services and their status
2. Monitor service states and health metrics
3. View service logs in real-time
4. Browse service configurations
5. Monitor system resources and service health

## Implementation Steps

### Phase 1: Project Setup
1. Initialize a Vite + React + TypeScript project
2. Configure necessary dependencies (Chakra UI, Redux Toolkit, etc.)
3. Set up the development environment

### Phase 2: Core Components
1. Create the main layout and navigation
2. Build the service list view
3. Implement status indicators and metrics
4. Add real-time status updates via file polling

### Phase 3: Advanced Features
1. Log viewer with real-time streaming (reading from log files)
2. Service configuration viewer (read-only)
3. System metrics dashboard
4. Error handling and browser compatibility

## Technical Requirements

- Use React 18+ with TypeScript
- Use Vite for build tooling
- Use Chakra UI for styling
- Implement File System Access API for reading state files
- Create a responsive design that works on desktop and mobile

## File System Integration

The UI reads directly from SystemG's state files on disk using the File System Access API:

State files to read:
- `~/.systemg/state/supervisor.pid` - Daemon PID
- `~/.systemg/state/services.state` - Service states (JSON)
- `~/.systemg/state/cron.state` - Cron job states (JSON)
- `~/.systemg/logs/*.log` - Service logs
- `~/.systemg/metrics/*` - Performance metrics

No backend server or API endpoints are used - this is a static HTML application.

## Output

Your final implementation should:
1. Be production-ready with proper error handling
2. Include unit tests for critical components
3. Have clear documentation
4. Follow React best practices
5. Be performant and responsive

Begin by setting up the project structure and implementing the core functionality.