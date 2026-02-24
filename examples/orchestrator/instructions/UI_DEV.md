# UI Developer Instructions

## TASK REJECTION RULES

If you are assigned a task that is NOT UI component creation, REJECT IT immediately:
- REJECT: File system services, polling, data fetching → belongs to core-infra-dev
- REJECT: Redux store, state management → belongs to features-dev
- REJECT: Testing tasks → belongs to qa-dev
- ACCEPT: ANY component creation (Dashboard, ProcessList, LogViewer, etc.)
- ACCEPT: Layout components, pages, routing

If the task title doesn't include "Component", "Layout", "Page", or "UI", be suspicious and verify it belongs to you.

## CRITICAL: Build Working Components First

Listen carefully - the number one priority is to build React components that actually render in the browser. Do not write tests for components that don't exist. Do not write documentation for code that doesn't exist. Do not optimize what doesn't work yet.

Your deliverables are measured by working code that displays in the browser when someone runs `yarn dev` and opens localhost:5173. Nothing else matters until that works.

## Build Order - Follow This Exactly

1. Create the component file as a .tsx in src/components/
2. Write the component with mock data hardcoded inside it
3. Import it in App.tsx and render it
4. Run yarn dev and verify it displays at localhost:5173
5. Only after you see it working in the browser, then write tests

If the component doesn't render in the browser, you have not completed the task. Period.

## CRITICAL FILE LOCATION RULE
ALL files you create MUST go inside the orchestrator-ui/ folder. Never create files outside this directory. No files in parent directories, no files in sibling directories. Everything goes inside orchestrator-ui/.

## Role
Build the complete visual interface for SystemG monitoring dashboard. Create functional React components first, make them pretty second, make them perfect third.

## Working Directory
ALL your work happens inside: `orchestrator-ui/`
- Components go in: `orchestrator-ui/src/components/`
- App.tsx is at: `orchestrator-ui/src/App.tsx`
- Never create files outside orchestrator-ui/ folder

## Design System Foundation

### Theme Configuration
```typescript
// theme/index.ts
const theme = {
  colors: {
    // Dark mode (default)
    bg: {
      primary: '#1a1a1a',
      secondary: '#242424',
      tertiary: '#2d2d2d'
    },
    text: {
      primary: 'rgba(255, 255, 255, 0.87)',
      secondary: 'rgba(255, 255, 255, 0.60)',
      muted: 'rgba(255, 255, 255, 0.38)'
    },
    status: {
      running: '#00c896',  // Green
      stopped: '#6b7280',  // Gray
      error: '#ef4444',    // Red
      warning: '#f59e0b',  // Yellow
      starting: '#3b82f6'  // Blue
    }
  },
  fonts: {
    body: 'Inter, system-ui, sans-serif',
    mono: 'Fira Code, Consolas, monospace'  // For logs, metrics
  },
  spacing: {
    xs: '0.25rem',
    sm: '0.5rem',
    md: '1rem',
    lg: '1.5rem',
    xl: '2rem'
  }
};
```

## Component Specifications

### BUILD THESE IN ORDER - DO NOT SKIP AHEAD

Start with component #1. Make it work. See it in the browser. Then move to #2. Do not write ten test files before you have a single working component.

### 1. Application Shell (App.tsx)

Start here. Make App.tsx render something more than placeholder text. Use this structure but with hardcoded mock data first:

```typescript
// START SIMPLE - just get components rendering
function App() {
  // Hardcode this to true initially to skip directory picker
  const hasDirectory = true;

  return (
    <ChakraProvider>
      <div className="systemg-app">
        {!hasDirectory ? (
          <div>Directory Picker Placeholder</div>
        ) : (
          <>
            <h1>SystemG Dashboard</h1>
            <Dashboard />
          </>
        )}
      </div>
    </ChakraProvider>
  );
}
```

Run yarn dev. See it in browser. Only then continue.

### 2. Directory Picker Component

Initial screen for selecting SystemG directory:

```typescript
interface DirectoryPickerProps {
  onSelect: (handle: FileSystemDirectoryHandle) => void;
  onFallback: (file: File) => void;  // For Firefox/Safari
}
```

Requirements:
- Large, centered call-to-action button
- Browser compatibility detection
- Show different UI for unsupported browsers
- Instructions for running `systemg export`
- Drag-and-drop zone for manual upload
- Error handling for invalid directories

Visual specs:
```
┌─────────────────────────────────────┐
│                                     │
│     Select SystemG Directory        │
│                                     │
│    ┌─────────────────────────┐      │
│    │   📁 Choose Directory    │      │
│    └─────────────────────────┘      │
│                                     │
│    --- or for Firefox/Safari ---    │
│                                     │
│    Drop systemg-export.tar.gz here  │
│                                     │
└─────────────────────────────────────┘
```

### 3. Dashboard Component - BUILD THIS SECOND

Create src/components/Dashboard.tsx with HARDCODED DATA first:

```typescript
// src/components/Dashboard.tsx
// START WITH THIS - WORKING CODE WITH FAKE DATA
export function Dashboard() {
  // HARDCODE DATA - don't worry about props yet
  const runningCount = 24;
  const cpuUsage = 45;
  const memoryUsage = "1.2GB";
  const uptime = "3d 4h";

  return (
    <div className="dashboard">
      <div className="quick-stats">
        <div className="stat-card">
          <div className="stat-label">Running</div>
          <div className="stat-value">{runningCount}</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">CPU</div>
          <div className="stat-value">{cpuUsage}%</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Memory</div>
          <div className="stat-value">{memoryUsage}</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Uptime</div>
          <div className="stat-value">{uptime}</div>
        </div>
      </div>
    </div>
  );
}
```

Import this in App.tsx. Run yarn dev. Verify you see the stats. ONLY THEN worry about making it pretty or adding props.

### 4. Process List Component

Display all processes with tree structure:

```typescript
interface ProcessListProps {
  processes: ProcessInfo[];
  onSelect: (process: ProcessInfo) => void;
  searchTerm: string;
  filters: FilterState;
}
```

Features:
- Collapsible tree structure
- Status badges with colors
- Real-time CPU/Memory sparklines
- Search highlighting
- Sort by name/cpu/memory/status
- Keyboard navigation (j/k for up/down)

Visual:
```
┌──────────────────────────────────────────────┐
│ 🔍 Search...              ▼ Status  ▼ Sort   │
├──────────────────────────────────────────────┤
│ ▶ nginx           [RUNNING] CPU: ▁▃▅▂ Mem: 45MB│
│ ▼ postgres        [RUNNING] CPU: ▅▆▇▄ Mem: 320MB│
│   └ worker-1      [RUNNING] CPU: ▂▁▂▁ Mem: 89MB│
│   └ worker-2      [STOPPED] CPU: ---- Mem: 0MB │
│ ▶ redis           [ERROR]   CPU: ---- Mem: 0MB │
└──────────────────────────────────────────────┘
```

### 5. Process Details Panel

Detailed view when process is selected:

```typescript
interface ProcessDetailsProps {
  process: ProcessInfo;
  logs: LogEntry[];
  metrics: ProcessMetrics;
  onAction: (action: 'start' | 'stop' | 'restart') => void;
}
```

Layout:
```
┌─────────────────────────────────────────────┐
│ nginx (PID: 1234)              [✓ RUNNING]  │
├─────────────────────────────────────────────┤
│ Command: /usr/bin/nginx -g daemon off;      │
│ Uptime: 3 days, 4 hours                     │
│ Restart Count: 2                             │
│                                              │
│ [Restart] [Stop] [View Config]              │
├─────────────────────────────────────────────┤
│ Resources                                    │
│ CPU:  ████░░░░░░ 45%  Peak: 78%             │
│ MEM:  ██░░░░░░░░ 234MB / 2GB                │
├─────────────────────────────────────────────┤
│ Environment (sanitized)                      │
│ NODE_ENV: production                         │
│ API_KEY: sk-****                            │
└─────────────────────────────────────────────┘
```

### 6. Log Viewer Component

Real-time log streaming with virtual scrolling:

```typescript
interface LogViewerProps {
  logs: LogEntry[];
  autoScroll: boolean;
  searchTerm: string;
  levelFilter: LogLevel[];
}
```

Requirements:
- Virtual scrolling for performance (react-window)
- Color-coded log levels
- Search with highlighting
- Auto-scroll toggle
- Jump to timestamp
- Copy button for selections
- Show truncation warnings for large files

Visual:
```
┌─────────────────────────────────────────────┐
│ Logs │ 🔍 Search │ Levels: ■INFO ■WARN ■ERR │
├─────────────────────────────────────────────┤
│[14:23:01] INFO  Server started on port 3000  │
│[14:23:02] DEBUG Connected to database        │
│[14:23:05] WARN  High memory usage: 89%       │
│[14:23:08] ERROR Failed to connect: timeout   │
│   Stack trace:                               │
│     at connect() line 45                     │
│[14:23:10] INFO  Retrying connection...       │
├─────────────────────────────────────────────┤
│ ⚠ Log truncated (showing last 1MB)   [▼ Auto]│
└─────────────────────────────────────────────┘
```

### 7. Metrics Charts Component

ASCII-art style performance graphs:

```typescript
interface MetricsChartProps {
  data: MetricPoint[];
  type: 'cpu' | 'memory' | 'network';
  period: '1h' | '6h' | '24h';
  height: number;  // lines
}
```

ASCII chart example:
```
CPU Usage (%)
100 ┤
 90 ┤    ╭─╮
 80 ┤   ╱  ╰╮
 70 ┤  ╱    ╰─╮
 60 ┤ ╱       ╰╮
 50 ┤╱         ╰──────╮
 40 ┤               ╰─────
    └────────────────────
     -1h    -30m     Now
```

### 8. Cron Jobs View

Display scheduled tasks:

```typescript
interface CronViewProps {
  jobs: CronJob[];
  history: CronExecution[];
}
```

Layout:
```
┌─────────────────────────────────────────────┐
│ Scheduled Jobs                              │
├─────────────────────────────────────────────┤
│ Name      Schedule    Next Run    Last Status│
│ backup    0 * * * *   in 5 min    ✓ Success │
│ cleanup   0 0 * * *   in 1 hr     ✓ Success │
│ report    0 9 * * 1   Monday      ⚠ Warning │
└─────────────────────────────────────────────┘
```

### 9. Configuration Viewer

YAML config display with syntax highlighting:

```typescript
interface ConfigViewerProps {
  config: string;  // YAML content
  readOnly: true;  // Always read-only
  onCopy: () => void;
}
```

Features:
- Syntax highlighting (use prism.js)
- Line numbers
- Search within config
- Copy button
- Collapse/expand sections

### 10. Status Bar Component

Bottom bar showing connection status:

```typescript
interface StatusBarProps {
  connected: boolean;
  lastPoll: number;
  pollingError?: string;
  degradedMode: boolean;
}
```

Visual:
```
┌─────────────────────────────────────────────┐
│ ● Connected │ Last update: 2s ago │ v1.0.0  │
└─────────────────────────────────────────────┘
```

## Accessibility Requirements

### Keyboard Navigation
```javascript
// Implement these shortcuts globally
const keyboardShortcuts = {
  '/': 'Focus search',
  'j': 'Next item',
  'k': 'Previous item',
  'Enter': 'Select item',
  'Escape': 'Close modal/Clear search',
  'g h': 'Go home',
  'g p': 'Go to processes',
  'g l': 'Go to logs',
  '?': 'Show help'
};
```

### ARIA Requirements
Every component must have:
- Proper roles (`role="navigation"`, `role="main"`)
- Labels (`aria-label` for icons)
- Live regions for updates (`aria-live="polite"`)
- Focus management in modals
- Skip links for navigation

### Focus Management
```typescript
// utils/focus.ts
export function trapFocus(container: HTMLElement) {
  const focusable = container.querySelectorAll(
    'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
  );
  const first = focusable[0];
  const last = focusable[focusable.length - 1];

  // Trap focus within container
}
```

## Performance Requirements

### Virtual Scrolling
Use `react-window` for:
- Process lists > 50 items
- Log viewer (always)
- Metrics with > 1000 data points

### Memoization
```typescript
// Expensive components must be memoized
const ProcessTree = React.memo(({ processes }) => {
  // Component logic
}, (prev, next) => {
  // Custom comparison
  return prev.processes === next.processes;
});
```

### Lazy Loading
```typescript
// Split bundles for heavy components
const MetricsView = lazy(() => import('./MetricsView'));
const ConfigEditor = lazy(() => import('./ConfigEditor'));
```

## Component Props Interface

All components must follow this pattern:

```typescript
interface ComponentProps {
  // Data props (from Redux)
  data: TypedData;

  // UI state props
  isLoading?: boolean;
  error?: Error;

  // Event handlers
  onAction?: (action: Action) => void;

  // Accessibility
  ariaLabel?: string;
  role?: string;

  // Performance
  virtualized?: boolean;
  debounceMs?: number;

  // Styling
  className?: string;
  sx?: ChakraStyleProps;
}
```

## Testing Requirements

Each component needs:
```typescript
// ComponentName.test.tsx
describe('ComponentName', () => {
  it('renders without crashing', () => {});
  it('displays data correctly', () => {});
  it('handles user interactions', () => {});
  it('is accessible', () => {
    // Check ARIA attributes
    // Test keyboard navigation
  });
  it('performs well', () => {
    // Measure render time
    // Check for memory leaks
  });
});
```

## Delivery Checklist

For each component, IN THIS ORDER:
1. [ ] Component file exists in src/components/
2. [ ] Component renders with mock data
3. [ ] Component imported and used in App.tsx
4. [ ] yarn dev shows component in browser at localhost:5173
5. [ ] Component displays meaningful content (not placeholder text)

Only after ALL above are checked:
6. [ ] TypeScript interfaces defined properly
7. [ ] Styling applied (Chakra UI or CSS)
8. [ ] Tests written for existing functionality
9. [ ] Props accept external data
10. [ ] Error states handled

## Integration Points

Your components will receive data from:
- **Redux store** (via useSelector hooks)
- **File API service** (via Redux actions)

Your components will send events to:
- **Redux actions** (user interactions)
- **Analytics service** (usage tracking)

## Success Criteria

Your UI is complete when:
1. yarn dev runs without errors
2. Browser at localhost:5173 shows actual dashboard with data (even if mock data)
3. All 10 components listed above exist and render
4. No "Development environment ready" placeholder text
5. User can see and interact with the interface

Everything else is secondary to the above.

## Proof of Completion Required

You MUST provide evidence that components work:
1. Screenshot or description of what displays at localhost:5173
2. List of component files created in src/components/
3. Confirmation that App.tsx imports and uses the components
4. yarn build completes successfully

If yarn dev doesn't show a working dashboard, the task is not complete.

## Common Failure Patterns to Avoid

Do not do these things that caused previous failure:
- Writing 2000 lines of tests before creating a single component
- Building services and infrastructure without UI to use them
- Creating empty component directories with no actual components
- Leaving App.tsx with only placeholder text
- Focusing on test coverage metrics instead of working features
