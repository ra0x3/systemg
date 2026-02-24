# Features Developer Instructions

## TASK REJECTION RULES

If you are assigned a task for UI component creation, REJECT IT immediately:
- REJECT: "Build Resource Monitor Component" → belongs to ui-dev
- REJECT: ANY component creation → belongs to ui-dev
- REJECT: File system services → belongs to core-infra-dev
- REJECT: Testing tasks → belongs to qa-dev
- ACCEPT: Redux store setup, state slices
- ACCEPT: Mock data services (ONLY after components exist)

Your job is state management, not building UI or infrastructure.

## CRITICAL: Only Build State Management for Existing Components

Do not create Redux slices for components that don't exist. Do not build state management for imaginary features. If there's no ProcessList component rendering in the browser, don't create a processListSlice.

Before building any Redux feature:
1. Verify the UI component exists in src/components/
2. Run yarn dev and see it rendering with mock data
3. Only then create the Redux slice to replace mock data

## CRITICAL FILE LOCATION RULE
ALL files you create MUST go inside the orchestrator-ui/ folder. Never create files outside this directory. No files in parent directories, no files in sibling directories. Everything goes inside orchestrator-ui/.

## Role
Implement state management for EXISTING UI components. Build the bridge between File API and components that are already rendering in the browser.

## Primary Reference
Review `docs/SYSTEMG_UI_SPEC.md` sections on:
- Redux state shape (lines 265-276, 460-507)
- Polling hooks (lines 708-787)
- Data sanitization (lines 816-875)
- Performance requirements (throughout)

## Working Directory
ALL your work happens inside: `orchestrator-ui/`
- Store files go in: `orchestrator-ui/src/store/`
- Hooks go in: `orchestrator-ui/src/hooks/`
- Utils go in: `orchestrator-ui/src/utils/`
- Never create files outside orchestrator-ui/ folder

## Architecture Overview

Your layer sits between:
- **Input**: File API service (from CORE_INFRA_DEV)
- **Output**: Redux store consumed by UI components (from UI_DEV)

Data flow:
```
File System → File API → Sanitizer → Redux Store → UI Components
     ↑                         ↓
     └──── Polling Loop ────────┘
```

## Build Order - Follow Component Development

1. Wait for UI developer to build Dashboard component with mock data
2. Create minimal Redux slice for Dashboard's data needs
3. Connect Dashboard to Redux instead of mock data
4. Verify Dashboard still renders
5. Move to next component only after current one works

## PRIORITY 1: Incremental Redux Store Setup

Start simple. Don't build all slices at once:

### Initial Store Structure (Start Here)
```typescript
// store/index.ts - START WITH ONLY WHAT'S NEEDED
import { configureStore } from '@reduxjs/toolkit';

export const store = configureStore({
  reducer: {
    // Add slices ONLY as components need them
    dashboard: dashboardReducer,  // Only if Dashboard component exists
    // Don't add these until components exist:
    // services: servicesReducer,
    // logs: logsReducer,
    // etc.
  },
  middleware: (getDefaultMiddleware) =>
    getDefaultMiddleware({
      serializableCheck: {
        // File handles are non-serializable
        ignoredActions: ['system/setDirectoryHandle'],
        ignoredPaths: ['system.directoryHandle']
      }
    })
});

export type RootState = ReturnType<typeof store.getState>;
export type AppDispatch = typeof store.dispatch;
```

### Complete State Shape
```typescript
interface SystemGState {
  // Core data from disk
  services: {
    processes: Process[];
    tree: ProcessTreeNode[];
    selectedId: string | null;
    lastUpdate: number;
  };

  supervisor: {
    info: SupervisorInfo | null;
    pid: number | null;
    status: 'running' | 'stopped' | 'unknown';
    uptime: number;
  };

  cron: {
    jobs: CronJob[];
    history: CronExecution[];
    nextRuns: ScheduledRun[];
  };

  logs: {
    entries: LogLine[];        // Last 10,000 lines
    offsets: Record<string, number>;  // File positions
    filters: {
      level: LogLevel[];
      service: string | null;
      searchTerm: string;
    };
    autoScroll: boolean;
  };

  metrics: {
    current: MetricsSnapshot;
    history: MetricsPoint[];   // Last hour, downsampled
    aggregates: {
      cpu: { avg: number; max: number; };
      memory: { avg: number; max: number; };
    };
  };

  // System state
  system: {
    directoryHandle: FileSystemDirectoryHandle | null;
    browserSupport: {
      fileApi: boolean;
      browser: 'chrome' | 'firefox' | 'safari' | 'other';
      degradedMode: boolean;
    };
    polling: {
      isPolling: boolean;
      lastPoll: number;
      errorCount: number;
      errorMessage: string | null;
      backoffDelay: number;
    };
  };

  // UI state
  ui: {
    theme: 'dark' | 'light';
    sidebarCollapsed: boolean;
    activeView: 'dashboard' | 'processes' | 'logs' | 'metrics' | 'cron' | 'config';
    modals: {
      processDetails: { open: boolean; processId: string | null; };
      exportData: { open: boolean; format: 'json' | 'csv' | null; };
    };
    notifications: Notification[];
  };
}
```

## PRIORITY 2: Service Slices Implementation

### Services Slice
```typescript
// store/slices/servicesSlice.ts
import { createSlice, PayloadAction, createSelector } from '@reduxjs/toolkit';

const servicesSlice = createSlice({
  name: 'services',
  initialState: {
    processes: [] as Process[],
    tree: [] as ProcessTreeNode[],
    selectedId: null as string | null,
    lastUpdate: 0
  },
  reducers: {
    updateProcesses: (state, action: PayloadAction<Process[]>) => {
      state.processes = action.payload;
      state.tree = buildProcessTree(action.payload);
      state.lastUpdate = Date.now();
    },
    selectProcess: (state, action: PayloadAction<string | null>) => {
      state.selectedId = action.payload;
    },
    updateProcessStatus: (state, action: PayloadAction<{id: string; status: ProcessStatus}>) => {
      const process = state.processes.find(p => p.id === action.payload.id);
      if (process) {
        process.status = action.payload.status;
      }
    }
  }
});

// Memoized selectors for performance
export const selectProcessTree = createSelector(
  [(state: RootState) => state.services.tree],
  (tree) => tree
);

export const selectProcessById = createSelector(
  [(state: RootState) => state.services.processes, (_: RootState, id: string) => id],
  (processes, id) => processes.find(p => p.id === id)
);

export const selectRunningProcesses = createSelector(
  [(state: RootState) => state.services.processes],
  (processes) => processes.filter(p => p.status === 'running')
);
```

### Logs Slice with Streaming
```typescript
// store/slices/logsSlice.ts
const logsSlice = createSlice({
  name: 'logs',
  initialState: {
    entries: [] as LogLine[],
    offsets: {} as Record<string, number>,
    filters: {
      level: ['info', 'warn', 'error'] as LogLevel[],
      service: null as string | null,
      searchTerm: ''
    },
    autoScroll: true
  },
  reducers: {
    appendLogs: (state, action: PayloadAction<LogLine[]>) => {
      // Keep only last 10,000 lines for performance
      const newEntries = [...state.entries, ...action.payload];
      state.entries = newEntries.slice(-10000);
    },
    updateOffset: (state, action: PayloadAction<{file: string; offset: number}>) => {
      state.offsets[action.payload.file] = action.payload.offset;
    },
    setLogFilter: (state, action: PayloadAction<Partial<typeof state.filters>>) => {
      state.filters = { ...state.filters, ...action.payload };
    },
    clearLogs: (state) => {
      state.entries = [];
    }
  }
});
```

### Metrics Slice with Aggregation
```typescript
// store/slices/metricsSlice.ts
const metricsSlice = createSlice({
  name: 'metrics',
  initialState: {
    current: null as MetricsSnapshot | null,
    history: [] as MetricsPoint[],
    aggregates: {
      cpu: { avg: 0, max: 0 },
      memory: { avg: 0, max: 0 }
    }
  },
  reducers: {
    updateMetrics: (state, action: PayloadAction<MetricsSnapshot>) => {
      state.current = action.payload;

      // Add to history with downsampling
      const point: MetricsPoint = {
        timestamp: Date.now(),
        cpu: action.payload.totalCpu,
        memory: action.payload.totalMemory
      };

      // Downsample to 1-minute resolution if needed
      const lastPoint = state.history[state.history.length - 1];
      if (!lastPoint || point.timestamp - lastPoint.timestamp >= 60000) {
        state.history.push(point);
        // Keep only last hour (60 points)
        state.history = state.history.slice(-60);
      }

      // Update aggregates
      state.aggregates = calculateAggregates(state.history);
    }
  }
});
```

## PRIORITY 3: Polling Hook Implementation

Create the main hook that connects File API to Redux:

```typescript
// hooks/useSystemGPoller.ts
import { useEffect, useRef, useCallback } from 'react';
import { useDispatch, useSelector } from 'react-redux';
import { FileSystemAPI } from '../services/fileSystem';

export function useSystemGPoller(
  fileApi: FileSystemAPI,
  options: PollerOptions = {}
) {
  const dispatch = useDispatch();
  const pollingState = useSelector((state: RootState) => state.system.polling);
  const pollInterval = useRef<number>();
  const backoffDelay = useRef(1000);

  const poll = useCallback(async () => {
    if (pollingState.isPolling) return; // Single-flight

    dispatch(setPollingStatus({ isPolling: true }));

    try {
      const updates = await fileApi.readSnapshot();

      // Sanitize BEFORE dispatching
      const sanitized = {
        services: sanitizeServices(updates.services),
        logs: sanitizeLogs(updates.logs),
        metrics: updates.metrics,
        cron: updates.cron
      };

      // Batch dispatch for performance
      dispatch(batch(() => {
        dispatch(updateProcesses(sanitized.services));
        dispatch(appendLogs(sanitized.logs));
        dispatch(updateMetrics(sanitized.metrics));
        dispatch(updateCron(sanitized.cron));
        dispatch(updateLastPoll(Date.now()));
      }));

      // Reset backoff on success
      backoffDelay.current = 1000;
      dispatch(clearPollingError());

    } catch (error) {
      // Exponential backoff
      backoffDelay.current = Math.min(backoffDelay.current * 1.5, 30000);
      dispatch(setPollingError(error.message));
    } finally {
      dispatch(setPollingStatus({ isPolling: false }));
    }
  }, [fileApi, dispatch, pollingState.isPolling]);

  useEffect(() => {
    // Start polling
    const startPolling = () => {
      poll();
      pollInterval.current = window.setTimeout(startPolling, backoffDelay.current);
    };

    startPolling();

    // Cleanup
    return () => {
      if (pollInterval.current) {
        clearTimeout(pollInterval.current);
      }
    };
  }, [poll]);

  return {
    isPolling: pollingState.isPolling,
    lastPoll: pollingState.lastPoll,
    error: pollingState.errorMessage,
    retry: poll
  };
}
```

## PRIORITY 4: Search & Filter Implementation

Build efficient client-side search:

```typescript
// utils/search.ts
export class ProcessSearchIndex {
  private index: Map<string, Set<string>> = new Map();
  private processes: Process[] = [];

  build(processes: Process[]) {
    this.processes = processes;
    this.index.clear();

    processes.forEach(p => {
      // Index by name
      this.addToIndex(p.name.toLowerCase(), p.id);

      // Index by command
      p.command.toLowerCase().split(/\s+/).forEach(term => {
        this.addToIndex(term, p.id);
      });

      // Index by status
      this.addToIndex(p.status, p.id);
    });
  }

  search(query: string): Process[] {
    const terms = query.toLowerCase().split(/\s+/);
    const matches = new Set<string>();

    terms.forEach(term => {
      const ids = this.index.get(term);
      if (ids) {
        ids.forEach(id => matches.add(id));
      }
    });

    return this.processes.filter(p => matches.has(p.id));
  }

  private addToIndex(term: string, id: string) {
    if (!this.index.has(term)) {
      this.index.set(term, new Set());
    }
    this.index.get(term)!.add(id);
  }
}
```

### Filter System
```typescript
// utils/filters.ts
export interface FilterState {
  status: ProcessStatus[];
  cpuThreshold: number | null;
  memoryThreshold: number | null;
  namePattern: string | null;
}

export function applyFilters(processes: Process[], filters: FilterState): Process[] {
  return processes.filter(p => {
    // Status filter
    if (filters.status.length && !filters.status.includes(p.status)) {
      return false;
    }

    // CPU threshold
    if (filters.cpuThreshold !== null && p.cpu < filters.cpuThreshold) {
      return false;
    }

    // Memory threshold
    if (filters.memoryThreshold !== null && p.memory < filters.memoryThreshold) {
      return false;
    }

    // Name pattern
    if (filters.namePattern) {
      const regex = new RegExp(filters.namePattern, 'i');
      if (!regex.test(p.name)) {
        return false;
      }
    }

    return true;
  });
}
```

## PRIORITY 5: Export System

Implement data export with sanitization:

```typescript
// utils/export.ts
export class DataExporter {
  exportToJSON(state: RootState): Blob {
    const exportData = {
      timestamp: new Date().toISOString(),
      processes: this.sanitizeProcesses(state.services.processes),
      metrics: state.metrics.current,
      logs: state.logs.entries.slice(-1000), // Last 1000 lines
      cron: state.cron.jobs
    };

    const json = JSON.stringify(exportData, null, 2);
    return new Blob([json], { type: 'application/json' });
  }

  exportToCSV(processes: Process[]): Blob {
    const headers = ['Name', 'Status', 'PID', 'CPU %', 'Memory MB', 'Uptime'];
    const rows = processes.map(p => [
      p.name,
      p.status,
      p.pid || '',
      p.cpu.toFixed(2),
      (p.memory / 1024 / 1024).toFixed(2),
      p.uptime
    ]);

    const csv = [
      headers.join(','),
      ...rows.map(r => r.map(this.escapeCSV).join(','))
    ].join('\n');

    return new Blob([csv], { type: 'text/csv' });
  }

  private escapeCSV(value: string): string {
    if (value.includes(',') || value.includes('"') || value.includes('\n')) {
      return `"${value.replace(/"/g, '""')}"`;
    }
    return value;
  }

  private sanitizeProcesses(processes: Process[]): Process[] {
    return processes.map(p => ({
      ...p,
      env: this.sanitizeEnv(p.env)
    }));
  }

  private sanitizeEnv(env: Record<string, string>): Record<string, string> {
    const sanitized: Record<string, string> = {};
    const sensitiveKeys = /password|secret|token|key|credential/i;

    for (const [key, value] of Object.entries(env)) {
      if (sensitiveKeys.test(key)) {
        sanitized[key] = '***REDACTED***';
      } else {
        sanitized[key] = value;
      }
    }

    return sanitized;
  }
}
```

## PRIORITY 6: Performance Optimizations

### Memoized Selectors
```typescript
// store/selectors/index.ts
import { createSelector } from '@reduxjs/toolkit';

// Heavy computation memoized
export const selectProcessTreeWithMetrics = createSelector(
  [selectProcessTree, selectMetrics],
  (tree, metrics) => {
    // Expensive tree decoration
    return decorateTreeWithMetrics(tree, metrics);
  }
);

// Filtered logs memoized
export const selectFilteredLogs = createSelector(
  [
    (state: RootState) => state.logs.entries,
    (state: RootState) => state.logs.filters
  ],
  (entries, filters) => {
    return entries.filter(log => {
      if (!filters.level.includes(log.level)) return false;
      if (filters.service && log.service !== filters.service) return false;
      if (filters.searchTerm && !log.message.includes(filters.searchTerm)) return false;
      return true;
    });
  }
);
```

### Web Worker for Heavy Processing
```typescript
// workers/metricsWorker.ts
self.addEventListener('message', (event) => {
  const { type, data } = event.data;

  switch (type) {
    case 'AGGREGATE_METRICS':
      const aggregated = performHeavyAggregation(data);
      self.postMessage({ type: 'METRICS_AGGREGATED', data: aggregated });
      break;

    case 'DOWNSAMPLE_LOGS':
      const downsampled = downsampleLogs(data);
      self.postMessage({ type: 'LOGS_DOWNSAMPLED', data: downsampled });
      break;
  }
});

// Use in hook
export function useMetricsWorker() {
  const worker = useRef<Worker>();

  useEffect(() => {
    worker.current = new Worker('/workers/metricsWorker.js');
    return () => worker.current?.terminate();
  }, []);

  const aggregate = useCallback((data: MetricsData) => {
    return new Promise((resolve) => {
      worker.current!.onmessage = (e) => {
        if (e.data.type === 'METRICS_AGGREGATED') {
          resolve(e.data.data);
        }
      };
      worker.current!.postMessage({ type: 'AGGREGATE_METRICS', data });
    });
  }, []);

  return { aggregate };
}
```

## PRIORITY 7: Error Handling

Implement comprehensive error boundaries:

```typescript
// utils/errors.ts
export class SystemGError extends Error {
  constructor(
    message: string,
    public code: ErrorCode,
    public recoverable: boolean = true
  ) {
    super(message);
  }
}

export enum ErrorCode {
  FILE_ACCESS_DENIED = 'FILE_ACCESS_DENIED',
  INVALID_DIRECTORY = 'INVALID_DIRECTORY',
  PARSE_ERROR = 'PARSE_ERROR',
  NETWORK_ERROR = 'NETWORK_ERROR',
  QUOTA_EXCEEDED = 'QUOTA_EXCEEDED'
}

// Middleware for error handling
export const errorMiddleware: Middleware = (store) => (next) => (action) => {
  try {
    return next(action);
  } catch (error) {
    console.error('Redux error:', error);

    // Dispatch error action
    store.dispatch({
      type: 'error/occurred',
      payload: {
        message: error.message,
        code: error.code || 'UNKNOWN',
        timestamp: Date.now()
      }
    });

    // Re-throw if not recoverable
    if (error instanceof SystemGError && !error.recoverable) {
      throw error;
    }
  }
};
```

## Testing Requirements

### Store Tests
```typescript
// store/__tests__/servicesSlice.test.ts
describe('servicesSlice', () => {
  it('updates processes and builds tree', () => {
    const state = servicesReducer(undefined, updateProcesses(mockProcesses));
    expect(state.processes).toEqual(mockProcesses);
    expect(state.tree).toBeDefined();
  });

  it('maintains selection across updates', () => {
    let state = servicesReducer(undefined, selectProcess('proc-1'));
    state = servicesReducer(state, updateProcesses(mockProcesses));
    expect(state.selectedId).toBe('proc-1');
  });
});
```

### Hook Tests
```typescript
// hooks/__tests__/useSystemGPoller.test.ts
describe('useSystemGPoller', () => {
  it('polls at correct intervals', async () => {
    const { result } = renderHook(() => useSystemGPoller(mockFileApi));

    await act(async () => {
      await jest.advanceTimersByTime(1000);
    });

    expect(mockFileApi.readSnapshot).toHaveBeenCalledTimes(1);
  });

  it('implements exponential backoff on error', async () => {
    mockFileApi.readSnapshot.mockRejectedValue(new Error('Failed'));

    const { result } = renderHook(() => useSystemGPoller(mockFileApi));

    // First retry at 1.5s
    await act(async () => {
      await jest.advanceTimersByTime(1500);
    });

    // Second retry at 2.25s
    await act(async () => {
      await jest.advanceTimersByTime(2250);
    });

    expect(mockFileApi.readSnapshot).toHaveBeenCalledTimes(3);
  });
});
```

## Acceptance Criteria

Your feature layer is complete when:

1. **Redux Store Operational**
   - [ ] All slices implemented
   - [ ] TypeScript types complete
   - [ ] Selectors memoized
   - [ ] Middleware configured

2. **Data Flow Working**
   - [ ] Polling updates store
   - [ ] Sanitization applied
   - [ ] Error states handled
   - [ ] Performance targets met

3. **Search/Filter Functional**
   - [ ] Search returns <100ms
   - [ ] Filters apply correctly
   - [ ] Results update live
   - [ ] URL state synced

4. **Export System Ready**
   - [ ] JSON export works
   - [ ] CSV export works
   - [ ] Data sanitized
   - [ ] Large datasets handled

5. **Performance Optimized**
   - [ ] No memory leaks
   - [ ] 60fps maintained
   - [ ] Worker threads used
   - [ ] Selectors memoized

## Critical Success Factors

Remember:
- **Never** store raw secrets in Redux
- **Always** sanitize before storing
- **Use** selectors for derived data
- **Memoize** expensive computations
- **Test** all edge cases
- **Document** complex logic

Your code is the brain of the application. Make it smart, fast, and reliable.

## Artifact-Backed Delivery Requirements
- Do not mark data/store features complete unless real code lands in `orchestrator-ui/src/store/` or related feature modules.
- For each completion claim, include command evidence:
  - `npm run type-check`
  - `npm run test` (store/features scope or full suite)
- Report-only deliverables are insufficient for implementation tasks.
- Any failed feature task must be followed by a concrete remediation task with code changes and retest evidence.
