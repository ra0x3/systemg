# SystemG UI — Lightweight Dashboard Spec

## Overview

A clever, static web dashboard for [systemg](https://github.com/ra0x3/systemg) that reads state directly from disk. No backend, no server—just a static HTML file that polls sysg's existing state files.

Open `index.html`, point it at your `~/.systemg` directory, and watch your processes in real-time.

---

## Important: Branch Requirement

**Branch ownership sits with the Team Lead.** The repo does not mandate a
specific branch name.

- At kickoff, the Owner records the current branch under
  `./snapshots/active_branch`.
- The Team Lead updates that file whenever they change the base branch,
  communicating the change in status updates.
- All other agents read the file and check out the recorded branch before
  working:
  ```bash
  ACTIVE_BRANCH=$(cat ./snapshots/active_branch)
  git checkout "$ACTIVE_BRANCH"
  ```

Feature branches (`feature/<area>-<slug>`) should be cut from the recorded
branch and merged back into it. If the branch does not exist locally, create it
with `git checkout -B "$ACTIVE_BRANCH"`.

---

## Implementation

### Team Roles & Responsibilities

#### Team Lead
**Primary responsibility**: Code review, repository management, and final integration

**Tasks**:
- Review all PRs from developers before merging
- Perform final code commits to main branch
- Ensure code quality standards (TypeScript strict mode, no any types)
- Coordinate between team members
- Make architectural decisions when conflicts arise
- Deploy production build to CDN/static hosting
- Enforce poller performance budgets (single-flight reads, 1 s steady-state cadence, exponential backoff on errors)
- Guard data hygiene (only sanitized env data in Redux, verify log truncation caps are honoured)
- Ensure all bot commits use `git commit -m "<msg>" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`
- Require pushes via `git push https://systemg-bot:$YOUR_PAT@github.com/ra0x3/systemg.git` (PAT pulled from `.env`)
- Track token spend; block changes that increase prompt length needlessly

**Deliverables**:
- Approved and merged code
- Production releases
- Architecture decision records that document polling safeguards, sanitization points, and fallback flows
- Weekly token usage summaries shared with OWNER

#### Individual Contributors (Frontend Dev)
**Primary responsibility**: Implement UI components and core functionality

**Dev 1 - Core Infrastructure**:
- Set up Vite project with React 18 + TypeScript
- Implement Redux store with slices that only persist sanitized data
- Build the single-flight File API poller (no overlapping reads, exponential backoff, cached `lastModified`)
- Ship browser compatibility + manual snapshot fallback (unsupported browsers upload tarball from `systemg export`)
- Implement data transformation utilities and worker-based reducers for heavy JSON shaping
- Persist log offsets/metrics history in IndexedDB without leaking secrets

**Dev 2 - UI Components**:
- Build dashboard layout with Chakra UI
- Create process tree visualization
- Implement log viewer with virtual scrolling + chunked streaming (respect 1 MiB cap, show truncation warnings)
- Build ASCII sparkline charts for metrics backed by down-sampled worker data
- Add keyboard navigation (vim bindings)
- Surface polling/compatibility states (unsupported browser warning, degraded snapshot mode banner)

**Dev 3 - Features & Polish**:
- Implement search/filter functionality
- Add config viewer with syntax highlighting
- Create cron job scheduler view
- Build export functionality (CSV/JSON) that respects sanitization and truncation rules
- Implement dark/light theme toggle and remember preference without bloating prompts/config
- Instrument telemetry for polling duration, skipped snapshots, and token usage summaries surfaced to the Team Lead dashboard

**Submission process**:
1. Create feature branch from main (no direct commits to main)
2. Implement assigned features while enforcing poller guardrails, sanitization, and token-conscious copy
3. Run `npm run lint && npm run type-check && npm run test && npm run build`
4. Write unit/integration tests (≥80% coverage) covering happy path + failure modes (partial write, unsupported browser)
5. Commit with `git commit -m "<type>: <summary>" --author="systemg-bot <systemg-bot@users.noreply.github.com>"`
6. Push with `git push https://systemg-bot:$YOUR_PAT@github.com/ra0x3/systemg.git` (pull `$YOUR_PAT` from repo `.env`)
7. Submit code to QA for testing and respond to issues
8. Hand off to Team Lead for final review/merge once QA signs off

#### QA Tester
**Primary responsibility**: Validate functionality and ensure quality

**Test scenarios**:
- File API compatibility (Chrome/Edge happy path, Safari/Firefox fallback to manual snapshot upload)
- State polling performance with large datasets (1000+ processes, 10MB metrics blobs)
- UI responsiveness and frame budget while regex-searching 1 MiB log slices
- Memory usage during 1 hr sessions; ensure log truncation caps hold
- Error states (missing files, partial writes, permission denied, corrupt JSON)
- Keyboard navigation accessibility and screen reader announcements
- Token usage telemetry surfaces accurate counts to the Team Lead dashboard

**Process**:
1. Receive code from developers
2. Run test suite (unit + integration)
3. Perform manual testing per test plan
4. Document bugs with reproduction steps
5. Return to dev if issues found
6. Approve for Team Lead review when passing

**Testing checklist**:
- [ ] `npm run lint`, `type-check`, `test`, and `build` succeed
- [ ] No console errors in production build or worker threads
- [ ] File picker + manual snapshot fallback behave per spec
- [ ] Polling recovers after forced partial writes, file rotations, and permission changes
- [ ] UI updates reflect state changes without skipped frames (>55fps under load)
- [ ] Keyboard shortcuts and ARIA labels meet accessibility requirements
- [ ] Export/download functions respect sanitization + truncation
- [ ] Memory stays < 250 MB in Chrome Task Manager after 1 hr session
- [ ] Token usage estimates log to the Team Lead dashboard after each run

### Development Timeline

**Week 1**: Foundation
- Mon-Tue: Project setup, Redux store with sanitized slices (Dev 1)
- Wed: Implement single-flight poller + error backoff (Dev 1)
- Thu: Manual snapshot fallback + browser support matrix (Dev 1)
- Fri: Basic dashboard shell + compatibility banners (Dev 2)

**Week 2**: Core Features
- Mon: Process list & tree view with streaming updates (Dev 2)
- Tue: Log viewer chunked streaming + truncation warnings (Dev 2)
- Wed: Metrics worker + ASCII charts (Dev 3)
- Thu: Search/filter + config viewer w/ sanitization badges (Dev 3)
- Fri: Telemetry pipeline for polling stats & token usage (Dev 3)

**Week 3**: Polish & Testing
- Mon: Keyboard navigation, accessibility, theming (Dev 3)
- Tue: Export flows (CSV/JSON) with sanitization gates (Dev 3)
- Wed-Thu: QA sweep (performance, fallback, accessibility)
- Fri: Bug fixes + Team Lead dry run with token budget review

**Week 4**: Release
- Mon: Final QA pass on release candidate
- Tue: Production build optimization + bundle size audit
- Wed: Documentation updates (README, troubleshooting, manual snapshot guide)
- Thu: Deployment to static host + smoke test
- Fri: Launch + retro on performance budgets/token spend

---

## Design Philosophy

### Visual Direction
- **Clean & minimal** — focus on the data, not the chrome
- **Hacker aesthetic** — monospace fonts, ASCII art where it makes sense
- **Fast & responsive** — instant updates, no loading spinners
- **Dark mode by default** — easy on the eyes

### UX Philosophy
- **Zero friction** — open file, select directory, done
- **Glanceable status** — understand what's running instantly
- **Real-time** — poll state files every second
- **Keyboard-first** — vim-style shortcuts for power users

---

## Technical Stack

| Layer | Technology |
|-------|------------|
| Framework | React 18 + TypeScript |
| UI Library | Chakra UI (minimal theme) |
| Icons | Lucide React |
| State | Redux Toolkit |
| Data | File API (reads ~/.systemg/*) |
| Build | Vite |
| Backend | NONE - static HTML only |

---

## How It Works (No Backend!)

### SystemG Already Writes Everything We Need
SystemG continuously writes state to disk:
- `~/.systemg/state/supervisor.pid` - Daemon PID
- `~/.systemg/state/services.state` - Service states (JSON)
- `~/.systemg/state/cron.state` - Cron job states (JSON)
- `~/.systemg/logs/*.log` - Service logs
- `~/.systemg/metrics/*` - Performance metrics

### File API Polling
```typescript
// User selects their systemg directory
const dirHandle = await window.showDirectoryPicker();

const refreshIntervalMs = 1000;
let pollInFlight = false;
let pollTimer: number | undefined;
const lastSeenVersion = new Map<string, number>();

async function pollState() {
  if (pollInFlight) return; // never queue overlapping reads
  pollInFlight = true;

  try {
    const servicesHandle = await dirHandle.getFileHandle('state/services.state');
    const servicesFile = await servicesHandle.getFile();

    const servicesVersion = servicesFile.lastModified;
    if (lastSeenVersion.get('services') !== servicesVersion) {
      lastSeenVersion.set('services', servicesVersion);

      const servicesText = await servicesFile.text();
      const parsedServices = safeJsonParse(servicesText);

      if (parsedServices) {
        dispatch(updateServices(sanitizeServices(parsedServices)));
      }
    }
  } catch (error) {
    console.error('Polling error', error);
    dispatch(setPollingError(toPollingMessage(error)));
  } finally {
    pollInFlight = false;
    pollTimer = window.setTimeout(pollState, refreshIntervalMs);
  }
}

pollState();
```

```typescript
function safeJsonParse<T>(raw: string): T | null {
  try {
    return JSON.parse(raw) as T;
  } catch (error) {
    // Partial writes happen when systemg rewrites the file while we read it.
    // Retry on the next poll instead of crashing the UI.
    console.warn('Skipping malformed JSON snapshot', error);
    return null;
  }
}
```

**Polling guardrails**
- Always gate the poller with `pollInFlight` (or an `AbortController`) so large datasets cannot queue overlapping reads that hammer disk I/O.
- Cache the last `File.lastModified` (or a hash of the contents) and short-circuit unchanged snapshots—this keeps CPU usage low when nothing changes.
- Run sanitization before dispatching to Redux so sensitive environment variables never enter long-lived state; only short-lived local variables may touch raw values.
- On component unmount call `window.clearTimeout(pollTimer)` to stop polling immediately.
- When the browser throttles background tabs, back off by increasing `refreshIntervalMs` to avoid starving the main thread.

### Large snapshot handling
- Keep a per-file offset map for logs and metrics (`Map<string, number>`). Call `File.slice(offset)` so you only read the appended chunk instead of re-downloading multi-megabyte files.
- Cap log payloads: when a file grows beyond ~1 MiB, read only the trailing window and surface a UI hint that older lines were truncated for performance.
- Defer heavy parsing (`JSON.parse`, YAML highlighting) onto a `worker` when payloads exceed 256 KiB to prevent janking the main thread.
- Use `navigator.storage.estimate()` to detect when the browser sandbox is low on quota and prompt the user to clean up before continuing to stream logs.
- Persist the offset/state map in `IndexedDB` so reloads resume without hammering the filesystem.

### Redux State Shape
```typescript
interface SystemGState {
  services: ServiceState[];
  supervisor: SupervisorInfo;
  cron: CronJobState[];
  logs: LogEntry[];
  metrics: MetricsData;
  lastPoll: number;
}
```

---

## Core Features

### 1. Main Dashboard

Simple overview showing:
- **Process count** — How many things are running
- **CPU/Memory** — Total usage across all processes
- **Supervisor status** — Is sysg daemon alive?
- **Recent crashes** — Last 5 failed processes
- **Cron jobs** — Next scheduled runs

Components:
- `<QuickStats />` — Big numbers that matter
- `<ProcList />` — Flat list of all processes
- `<CronNext />` — What's running soon

---

### 2. Process Tree

Visual tree of processes and their spawn children:
- **PID & name** — Basic identifiers
- **Status indicator** — Green/red/yellow dots
- **CPU/Memory** — Live usage
- **Spawn children** — Nested tree view
- **Uptime** — How long it's been running

Interactions (READ-ONLY since we can't send commands):
- Click to expand/collapse children
- Double-click to see details
- Keyboard navigation (j/k vim-style)

---

### 3. Process Details

Click a process to see:
- **Command** — What's actually running
- **Sanitized environment** — Masked env vars only; raw values never leave the polling worker
- **Resource limits** — Configured limits
- **ASCII metrics** — CPU/Memory sparklines
- **Recent logs** — Last 100 lines
- **Exit codes** — History of crashes

---

### 4. Log Tail

Real-time log viewer:
- **Tail -f style** — Auto-scroll as new lines come in
- **Multiple streams** — stdout/stderr/supervisor
- **Search** — Regex search through logs
- **Jump to timestamp** — Navigate by time
- **Chunked reads** — Stream only appended data; cap payloads to prevent main-thread stalls

Implementation notes:
- Maintain a `Map<string, number>` of byte offsets per log file and call `file.slice(offset)`; skip work entirely if `file.size === offset`.
- Drop back to a worker thread (`Worker` + `TextDecoderStream`) for regex searching so the UI thread stays responsive during large scans.
- Apply sanitization before logs touch Redux; keep only the last 10 000 lines and expose a "download full log" action that uses the File API stream.

---

### 5. Config Viewer

Since we're read-only, show the current YAML config:
- **Syntax highlighting** — Colored YAML
- **Search** — Find services quickly
- **Copy button** — Copy config snippets
- **Validation indicators** — Show if config is valid

---

### 6. Metrics

Simple performance graphs:
- **ASCII charts** — Terminal-style graphs
- **CPU/Memory over time** — Last hour
- **Top consumers** — Which processes are hungry
- **Export** — Download as CSV

Implementation notes:
- Down-sample high-frequency metrics to 1 Hz before rendering; anything higher floods React with updates.
- Perform aggregation in a Web Worker so number crunching never blocks the main UI thread.
- Store only the past hour (3 600 points) in Redux and persist longer histories in IndexedDB for opt-in deep dives.

---

### 7. Cron Jobs

View scheduled tasks:
- **Next runs** — When stuff will execute
- **Last runs** — Recent execution history
- **Status** — Success/failure indicators

---

## Data Model

```typescript
// Complete type definitions for the SystemG UI

// Service/Process types
interface Process {
  name: string;
  pid: number;
  status: 'running' | 'stopped' | 'crashed' | 'starting' | 'stopping';
  command: string;
  startedAt: string;
  exitCode?: number;
  restartCount: number;

  // Resources
  cpuPercent: number;
  memoryMB: number;
  memoryLimit?: number;
  cpuLimit?: number;

  // Spawn tree
  parentPid?: number;
  children: Process[];

  // Configuration (sanitized before storing in Redux)
  sanitizedEnv: Record<string, string>;
  workDir: string;
  user?: string;
}

// Cron job definition
interface CronJob {
  id: string;
  name: string;
  schedule: string;
  command: string;
  lastRun?: {
    timestamp: string;
    exitCode: number;
    duration: number;
  };
  nextRun: string;
  status: 'active' | 'disabled' | 'running';
  failureCount: number;
}

// Log entry structure
interface LogLine {
  timestamp: string;
  level: 'debug' | 'info' | 'warn' | 'error';
  service: string;
  message: string;
  stream: 'stdout' | 'stderr' | 'supervisor';
}

// Metrics data point
interface MetricsData {
  timestamp: number;
  services: {
    [serviceName: string]: {
      cpu: number[];  // Time series data
      memory: number[];  // Time series data
      restarts: number;
      uptime: number;
    };
  };
  system: {
    totalCpu: number;
    totalMemory: number;
    loadAverage: [number, number, number];
  };
}

// Supervisor state
interface SupervisorInfo {
  pid: number;
  uptime: number;
  version: string;
  configPath: string;
  stateDir: string;
  logDir: string;
  startedAt: string;
}

// Service state from disk
interface ServiceState {
  name: string;
  pid?: number;
  status: string;
  exitCode?: number;
  startedAt?: string;
  stoppedAt?: string;
  restartCount: number;
  errorMessage?: string;
}

// Complete Redux state shape
interface SystemGState {
  // Core data
  services: Process[];
  supervisor: SupervisorInfo | null;
  cron: CronJob[];
  logs: LogLine[];
  metrics: MetricsData | null;

  // UI state
  selectedService: string | null;
  searchTerm: string;
  logFilter: 'all' | 'stdout' | 'stderr' | 'supervisor';
  timeWindow: '1h' | '6h' | '24h' | '7d';

  // System state
  lastPoll: number;
  pollingError: string | null;
  isPolling: boolean;
  directoryHandle: FileSystemDirectoryHandle | null;

  // User preferences
  theme: 'dark' | 'light';
  autoScroll: boolean;
  showMetrics: boolean;
  refreshInterval: number;
}

// Error types
interface SystemGError {
  type: 'FILE_ACCESS' | 'PARSE_ERROR' | 'PERMISSION_DENIED' | 'NOT_FOUND';
  message: string;
  timestamp: number;
  recoverable: boolean;
}
```

---

## Implementation

### Quick Start

```bash
# Build it
npm run build

# Open in browser
open dist/index.html

# Select your ~/.systemg directory when prompted
# Watch your processes!
```

### Browser Compatibility Check

```typescript
// browserCompat.ts
export interface FileApiSupport {
  supported: boolean;
  reason?: string;
}

export function checkFileAPISupport(): FileApiSupport {
  if (!('showDirectoryPicker' in window)) {
    return {
      supported: false,
      reason: 'File System Access API missing (Safari, Firefox ESR, hardened Chromium).',
    };
  }

  // Check for secure context (HTTPS or localhost)
  if (!window.isSecureContext) {
    return {
      supported: false,
      reason: 'Secure context required—serve over https:// or run on localhost.',
    };
  }

  return { supported: true };
}

export function getBrowserInfo() {
  const ua = navigator.userAgent;
  const fileApi = checkFileAPISupport();

  return {
    isChrome: /Chrome/.test(ua) && !/Edge/.test(ua),
    isFirefox: /Firefox/.test(ua),
    isSafari: /Safari/.test(ua) && !/Chrome/.test(ua),
    fileApi,
  };
}
```

- Support matrix: Chrome/Edge/Opera ≥ 86 fully support the File System Access API. Firefox, Brave in Tor mode, and Safari require a fallback flow (prompt users to upload a tarball generated by `systemg export`).
- Block the UI when `fileApi.supported` is `false` and surface `fileApi.reason`; otherwise users will interact with a broken dashboard without feedback.
- Offer a "manual snapshot" mode that accepts zipped state bundles so unsupported browsers still have read-only diagnostics.

### Redux Store Setup

```typescript
// store.ts
import { configureStore, createSlice, PayloadAction } from '@reduxjs/toolkit';
import type { SystemGState, Process, CronJob, LogLine, MetricsData, SupervisorInfo } from './types';

// Services slice
const servicesSlice = createSlice({
  name: 'services',
  initialState: [] as Process[],
  reducers: {
    updateServices: (state, action: PayloadAction<Process[]>) => action.payload,
    updateServiceStatus: (state, action: PayloadAction<{name: string; status: Process['status']}>) => {
      const service = state.find(s => s.name === action.payload.name);
      if (service) service.status = action.payload.status;
    },
  },
});

// Cron slice
const cronSlice = createSlice({
  name: 'cron',
  initialState: [] as CronJob[],
  reducers: {
    updateCron: (state, action: PayloadAction<CronJob[]>) => action.payload,
  },
});

// Logs slice
const logsSlice = createSlice({
  name: 'logs',
  initialState: [] as LogLine[],
  reducers: {
    appendLogs: (state, action: PayloadAction<LogLine[]>) => {
      // Keep only last 10000 lines for performance
      return [...state, ...action.payload].slice(-10000);
    },
    clearLogs: () => [],
  },
});

// Metrics slice
const metricsSlice = createSlice({
  name: 'metrics',
  initialState: null as MetricsData | null,
  reducers: {
    updateMetrics: (state, action: PayloadAction<MetricsData>) => action.payload,
  },
});

// System slice for UI and polling state
const systemSlice = createSlice({
  name: 'system',
  initialState: {
    supervisor: null as SupervisorInfo | null,
    lastPoll: 0,
    pollingError: null as string | null,
    isPolling: false,
    directoryHandle: null as FileSystemDirectoryHandle | null,
    selectedService: null as string | null,
    searchTerm: '',
    logFilter: 'all' as SystemGState['logFilter'],
    timeWindow: '1h' as SystemGState['timeWindow'],
    theme: 'dark' as SystemGState['theme'],
    autoScroll: true,
    showMetrics: true,
    refreshInterval: 1000,
  },
  reducers: {
    updateSupervisor: (state, action: PayloadAction<SupervisorInfo>) => {
      state.supervisor = action.payload;
    },
    setPollingError: (state, action: PayloadAction<string | null>) => {
      state.pollingError = action.payload;
    },
    setIsPolling: (state, action: PayloadAction<boolean>) => {
      state.isPolling = action.payload;
    },
    setDirectoryHandle: (state, action: PayloadAction<FileSystemDirectoryHandle | null>) => {
      state.directoryHandle = action.payload;
    },
    selectService: (state, action: PayloadAction<string | null>) => {
      state.selectedService = action.payload;
    },
    setSearchTerm: (state, action: PayloadAction<string>) => {
      state.searchTerm = action.payload;
    },
    setLogFilter: (state, action: PayloadAction<SystemGState['logFilter']>) => {
      state.logFilter = action.payload;
    },
    updateLastPoll: (state) => {
      state.lastPoll = Date.now();
    },
  },
});

// Configure store with all slices
export const store = configureStore({
  reducer: {
    services: servicesSlice.reducer,
    cron: cronSlice.reducer,
    logs: logsSlice.reducer,
    metrics: metricsSlice.reducer,
    system: systemSlice.reducer,
  },
  // Handle non-serializable FileSystemDirectoryHandle
  middleware: (getDefaultMiddleware) =>
    getDefaultMiddleware({
      serializableCheck: {
        ignoredActions: ['system/setDirectoryHandle'],
        ignoredPaths: ['system.directoryHandle'],
      },
    }),
});

// Export actions
export const { updateServices, updateServiceStatus } = servicesSlice.actions;
export const { updateCron } = cronSlice.actions;
export const { appendLogs, clearLogs } = logsSlice.actions;
export const { updateMetrics } = metricsSlice.actions;
export const {
  updateSupervisor,
  setPollingError,
  setIsPolling,
  setDirectoryHandle,
  selectService,
  setSearchTerm,
  setLogFilter,
  updateLastPoll,
} = systemSlice.actions;

// Export types
export type RootState = ReturnType<typeof store.getState>;
export type AppDispatch = typeof store.dispatch;
```

### File Polling Hook

```typescript
// useSystemGPoller.ts
import { useEffect, useRef } from 'react';
import { useDispatch } from 'react-redux';
import { updateServices, updateCron, updateMetrics, appendLogs, setPollingError } from './store';
import { sanitizeServices, readJsonSnapshot, readLogDelta, toPollingMessage } from './files';

const POLL_INTERVAL_MS = 1000;
const MAX_RETRY_DELAY_MS = 10_000;

export function useSystemGPoller(dirHandle: FileSystemDirectoryHandle | null) {
  const dispatch = useDispatch();
  const logOffsets = useRef(new Map<string, number>());

  useEffect(() => {
    if (!dirHandle) return undefined;

    let destroyed = false;
    let pollInFlight = false;
    let retryDelay = POLL_INTERVAL_MS;
    let timer: number | undefined;

    const scheduleNext = () => {
      if (destroyed) return;
      timer = window.setTimeout(pollState, retryDelay);
    };

    const pollState = async () => {
      if (pollInFlight || destroyed) return;
      pollInFlight = true;

      try {
        const services = await readJsonSnapshot(dirHandle, 'state/services.state');
        if (services) {
          dispatch(updateServices(sanitizeServices(services)));
        }

        const cron = await readJsonSnapshot(dirHandle, 'state/cron.state');
        if (cron) {
          dispatch(updateCron(cron));
        }

        const metrics = await readJsonSnapshot(dirHandle, 'metrics/latest.json');
        if (metrics) {
          dispatch(updateMetrics(metrics));
        }

        const newLogs = await readLogDelta(dirHandle, 'logs/supervisor.log', logOffsets.current, {
          maxBytes: 1_048_576, // 1 MiB safety cap
        });
        if (newLogs.length) {
          dispatch(appendLogs(newLogs));
        }

        retryDelay = POLL_INTERVAL_MS;
        dispatch(setPollingError(null));
      } catch (error) {
        console.error('Polling error', error);
        dispatch(setPollingError(toPollingMessage(error)));

        retryDelay = Math.min(retryDelay * 2, MAX_RETRY_DELAY_MS);
      } finally {
        pollInFlight = false;
        scheduleNext();
      }
    };

    pollState();

    return () => {
      destroyed = true;
      if (timer) {
        window.clearTimeout(timer);
      }
    };
  }, [dirHandle, dispatch]);
}
```

**Helper expectations**
- `readJsonSnapshot` memoizes each file's `lastModified` value so unchanged files short-circuit parsing. It must swallow partial writes and retry on the next tick instead of surfacing broken JSON to the UI.
- `readLogDelta` keeps a byte-offset map (`Map<string, number>`) and only returns the freshly appended log lines. Respect `maxBytes` to avoid loading multi-megabyte spikes into memory.
- `toPollingMessage` normalises DOMException names (e.g., `NotFoundError`, `NoModificationAllowedError`) into copy safe for user-facing alerts.
- Increase `retryDelay` only when errors occur; healthy steady-state polling should stay at `POLL_INTERVAL_MS`.

---

## Key Components

```tsx
// Simple status dot
<StatusDot status={proc.status} />

// ASCII sparkline
<SparkLine data={metrics} width={50} />

// Process tree node
<ProcNode proc={proc} depth={0} />

// Log viewer with search
<LogTail logs={logs} searchTerm={query} />
```

---

## Security Considerations

### Handling Sensitive Data

```typescript
// security.ts
const SENSITIVE_ENV_PATTERNS = [
  /api[_-]?key/i,
  /api[_-]?secret/i,
  /password/i,
  /passwd/i,
  /token/i,
  /auth/i,
  /credential/i,
  /private[_-]?key/i,
  /access[_-]?key/i,
  /secret[_-]?key/i,
  /aws[_-]?access/i,
  /aws[_-]?secret/i,
];

export function maskSensitiveValue(key: string, value: string): string {
  const isSensitive = SENSITIVE_ENV_PATTERNS.some(pattern => pattern.test(key));

  if (isSensitive) {
    // Show first 4 chars and mask the rest
    if (value.length <= 8) {
      return '********';
    }
    return value.substring(0, 4) + '*'.repeat(value.length - 4);
  }

  return value;
}

export function sanitizeEnvironment(env: Record<string, string>): Record<string, string> {
  const sanitized: Record<string, string> = {};

  for (const [key, value] of Object.entries(env)) {
    sanitized[key] = maskSensitiveValue(key, value);
  }

  return sanitized;
}

export function sanitizeLogs(logContent: string): string {
  // Remove potential secrets from logs
  let sanitized = logContent;

  // Replace JWT tokens
  sanitized = sanitized.replace(/Bearer\s+[A-Za-z0-9\-_=]+\.[A-Za-z0-9\-_=]+\.?[A-Za-z0-9\-_.+/=]*/g, 'Bearer [REDACTED]');

  // Replace API keys in URLs
  sanitized = sanitized.replace(/([?&])(api_key|apikey|token|auth)=([^&\s]+)/gi, '$1$2=[REDACTED]');

  // Replace basic auth in URLs
  sanitized = sanitized.replace(/:\/\/([^:]+):([^@]+)@/g, '://[REDACTED]:[REDACTED]@');

  return sanitized;
}
```

### Security Best Practices

1. **File Access Permissions**
   - Only request read permissions for systemg directory
   - Never write to system files from the UI
   - Validate all file paths before access

2. **Data Sanitization**
   - Mask sensitive environment variables before dispatch and persist them only in `sanitizedEnv`
   - Redact tokens and credentials from logs before appending to Redux
   - Never store raw secrets in Redux state or anywhere in IndexedDB/localStorage

3. **Browser Security**
   - Require HTTPS or localhost for File API
   - Use Content Security Policy headers
   - Sanitize all displayed data to prevent XSS

4. **Error Handling**
   ```typescript
   // errorBoundary.tsx
   export class SecurityErrorBoundary extends Component {
     componentDidCatch(error: Error) {
       // Never log full error with potentially sensitive data
       console.error('UI Error:', {
         message: error.message,
         type: error.name,
         // Don't log stack trace in production
         stack: process.env.NODE_ENV === 'development' ? error.stack : undefined
       });
     }
   }
   ```

---

## Build Setup & Project Structure

### Project Initialization

```bash
# Create project with Vite
npm create vite@latest orchestrator-ui -- --template react-ts
cd orchestrator-ui

# Install dependencies
npm install \
  @reduxjs/toolkit react-redux \
  @chakra-ui/react @emotion/react @emotion/styled framer-motion \
  lucide-react \
  @types/node

# Dev dependencies
npm install -D \
  @testing-library/react @testing-library/jest-dom \
  vitest jsdom \
  @typescript-eslint/eslint-plugin @typescript-eslint/parser \
  prettier eslint-config-prettier
```

### Project Structure

```
orchestrator-ui/
├── src/
│   ├── components/
│   │   ├── Dashboard/
│   │   │   ├── QuickStats.tsx
│   │   │   ├── ProcList.tsx
│   │   │   └── CronNext.tsx
│   │   ├── ProcessTree/
│   │   │   ├── ProcessNode.tsx
│   │   │   └── ProcessTree.tsx
│   │   ├── LogViewer/
│   │   │   ├── LogTail.tsx
│   │   │   └── LogSearch.tsx
│   │   ├── Metrics/
│   │   │   ├── SparkLine.tsx
│   │   │   └── MetricsChart.tsx
│   │   └── Common/
│   │       ├── StatusDot.tsx
│   │       ├── ErrorBoundary.tsx
│   │       └── FilePicker.tsx
│   ├── hooks/
│   │   ├── useSystemGPoller.ts
│   │   ├── useKeyboardNav.ts
│   │   └── useFileAPI.ts
│   ├── store/
│   │   ├── index.ts
│   │   ├── slices/
│   │   │   ├── services.ts
│   │   │   ├── cron.ts
│   │   │   ├── logs.ts
│   │   │   └── metrics.ts
│   │   └── types.ts
│   ├── utils/
│   │   ├── security.ts
│   │   ├── browserCompat.ts
│   │   ├── dataTransform.ts
│   │   └── formatters.ts
│   ├── App.tsx
│   ├── main.tsx
│   └── types.d.ts
├── public/
│   └── favicon.ico
├── tests/
│   ├── unit/
│   └── integration/
├── .env.example
├── vite.config.ts
├── tsconfig.json
├── package.json
└── README.md
```

### Vite Configuration

```typescript
// vite.config.ts
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'path';

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': resolve(__dirname, './src'),
      '@components': resolve(__dirname, './src/components'),
      '@hooks': resolve(__dirname, './src/hooks'),
      '@store': resolve(__dirname, './src/store'),
      '@utils': resolve(__dirname, './src/utils'),
    },
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: false,
    minify: 'esbuild',
    rollupOptions: {
      output: {
        manualChunks: {
          'vendor': ['react', 'react-dom', 'react-redux'],
          'ui': ['@chakra-ui/react', '@emotion/react'],
        },
      },
    },
  },
  server: {
    port: 3000,
    open: true,
  },
});
```

### TypeScript Configuration

```json
// tsconfig.json
{
  "compilerOptions": {
    "target": "ES2022",
    "useDefineForClassFields": true,
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,

    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "noEmit": true,
    "jsx": "react-jsx",

    "paths": {
      "@/*": ["./src/*"],
      "@components/*": ["./src/components/*"],
      "@hooks/*": ["./src/hooks/*"],
      "@store/*": ["./src/store/*"],
      "@utils/*": ["./src/utils/*"]
    }
  },
  "include": ["src"],
  "references": [{ "path": "./tsconfig.node.json" }]
}
```

### Package.json Scripts

```json
{
  "name": "orchestrator-ui",
  "version": "1.0.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc && vite build",
    "preview": "vite preview",
    "test": "vitest",
    "test:ui": "vitest --ui",
    "test:coverage": "vitest --coverage",
    "lint": "eslint src --ext ts,tsx",
    "format": "prettier --write 'src/**/*.{ts,tsx,css}'",
    "type-check": "tsc --noEmit"
  }
}
```

### Development Commands

```bash
# Start development server
npm run dev

# Run tests
npm run test

# Build for production
npm run build

# Preview production build
npm run preview

# Type checking
npm run type-check

# Linting & formatting
npm run lint
npm run format
```

### Deployment

```bash
# Build production bundle
npm run build

# The dist/ folder contains static files ready for deployment
# Can be served from any static file host (Netlify, Vercel, GitHub Pages, etc.)

# For local testing
npx serve dist

# Or simply open the index.html
open dist/index.html
```

### Minimum Viable Dashboard Timeline

1. **Day 1**: Project setup + File API integration
   - Initialize project structure
   - Set up Redux store
   - Implement file picker and browser compatibility

2. **Day 2**: Core UI Components
   - Build dashboard layout
   - Implement process list and tree view
   - Add status indicators

3. **Day 3**: Data Visualization
   - Log viewer with virtual scrolling
   - Metrics sparklines and charts
   - Real-time updates

4. **Day 4**: Polish & Features
   - Keyboard navigation
   - Search and filtering
   - Theme switching
   - Error boundaries

5. **Day 5**: Testing & Deployment
   - Unit tests for critical paths
   - Performance optimization
   - Production build
   - Deploy to static host

---

## Why This Is Cool

- **Zero backend complexity** — Just a static HTML file
- **File API magic** — Direct filesystem access from browser
- **Redux for state** — Clean, predictable state management
- **Real-time updates** — Poll every second, always fresh
- **Keyboard navigation** — vim bindings because we're not savages
- **ASCII art graphs** — Terminal aesthetic in the browser
- **Read-only safety** — Can't break anything, just observe

## Summary

A lightweight, clever dashboard that reads SystemG's state files directly from disk. No backend, no server processes, no complexity—just open an HTML file and watch your processes.

Built with React, Redux, and the File API. Polls state files every second for real-time updates.

**Total complexity**: One static HTML file + some JavaScript.
**Setup time**: 0 seconds.
**Maintenance burden**: None.
