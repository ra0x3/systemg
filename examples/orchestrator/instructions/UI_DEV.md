# UI Developer Instructions

## Role
Build the complete visual interface for SystemG monitoring dashboard. Create reusable, accessible, and performant components that display real-time system state.

## Primary Reference
Review `docs/SYSTEMG_UI_SPEC.md` sections on:
- Core features (lines 279-375)
- Component examples (lines 799-815)
- Visual direction (lines 157-170)
- Accessibility requirements (throughout)

## Working Directory
`orchestrator-ui/src/components/` and `orchestrator-ui/src/App.tsx`

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

### 1. Application Shell (App.tsx)

Main application layout that orchestrates all components:

```typescript
// Expected structure:
<ThemeProvider>
  <ChakraProvider>
    <Provider store={store}>
      <div className="systemg-app">
        {!directoryHandle ? (
          <DirectoryPicker onSelect={handleDirectory} />
        ) : (
          <>
            <Header />
            <MainLayout>
              <Sidebar />
              <ContentArea>
                <Dashboard />
              </ContentArea>
            </MainLayout>
            <StatusBar />
          </>
        )}
      </div>
    </Provider>
  </ChakraProvider>
</ThemeProvider>
```

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

### 3. Dashboard Component

Main overview with system statistics:

```typescript
interface DashboardProps {
  processes: ProcessInfo[];
  supervisor: SupervisorInfo;
  cron: CronJob[];
  metrics: SystemMetrics;
}
```

Layout:
```
┌─────────────────────────────────────────────┐
│  Quick Stats                                │
│  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐│
│  │ Running │ │  CPU   │ │ Memory │ │ Uptime ││
│  │   24    │ │  45%   │ │ 1.2GB  │ │ 3d 4h  ││
│  └────────┘ └────────┘ └────────┘ └────────┘│
│                                             │
│  Recent Issues               Next Cron Jobs │
│  ┌─────────────────────┐    ┌──────────────┐│
│  │ ⚠ nginx crashed     │    │ backup  5min ││
│  │ ⚠ redis high memory │    │ cleanup 1hr  ││
│  └─────────────────────┘    └──────────────┘│
└─────────────────────────────────────────────┘
```

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

For each component, ensure:
- [ ] TypeScript interfaces defined
- [ ] Chakra UI theme applied
- [ ] Dark mode works (default)
- [ ] Light mode works (optional)
- [ ] Responsive on mobile/tablet
- [ ] Keyboard navigable
- [ ] ARIA labels present
- [ ] Virtual scrolling for large lists
- [ ] Memoized for performance
- [ ] Unit tests written
- [ ] No console errors
- [ ] Props documented

## Integration Points

Your components will receive data from:
- **Redux store** (via useSelector hooks)
- **File API service** (via Redux actions)

Your components will send events to:
- **Redux actions** (user interactions)
- **Analytics service** (usage tracking)

## Success Criteria

Your UI is complete when:
1. All components render with mock data
2. Responsive design works on all screen sizes
3. Dark theme applied consistently
4. Keyboard navigation fully functional
5. Screen reader compatible
6. 60fps scrolling performance
7. <100ms interaction response time
8. All tests passing

Remember: The UI is what users see. Make it beautiful, fast, and accessible. Terminal aesthetic with modern polish.

## Artifact-Backed Delivery Requirements
- Every completed UI task must include concrete component code in `orchestrator-ui/src/`, not just design notes.
- When implementing screens, ensure the app renders meaningful dashboard content; placeholder text is not acceptable.
- Provide proof via commands and outcomes:
  - `npm run type-check`
  - `npm run test` (or component subset)
  - `npm run build`
- If blocked, produce a minimal reproducible blocker report and leave partial working code; do not mark task done with narrative only.
