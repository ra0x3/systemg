# Core Infrastructure Developer Instructions

## Role
Build the foundational infrastructure layer that enables the UI to read SystemG state files directly from disk. This is THE MOST CRITICAL component - without it, nothing else works.

## Primary Reference
Review `docs/SYSTEMG_UI_SPEC.md` sections on:
- File API implementation (lines 198-286)
- Browser compatibility (lines 527-570)
- Security considerations (lines 816-909)
- Large snapshot handling (lines 258-264)

## Working Directory
`orchestrator-ui/src/services/` and `orchestrator-ui/src/utils/`

## Critical Requirements

### PRIORITY 1: File System Access API Implementation

You must build the core file reading service that allows the browser to access `~/.systemg` files:

#### Browser Compatibility Layer
```typescript
// services/fileSystem.ts
interface FileSystemService {
  // Check if File System Access API is available
  isSupported(): boolean;

  // Get browser capabilities
  getBrowserInfo(): BrowserInfo;

  // Request directory access from user
  requestDirectory(): Promise<FileSystemDirectoryHandle | null>;

  // Read file with caching and change detection
  readFile(path: string): Promise<FileContent>;

  // Monitor file changes
  watchFile(path: string, callback: (content: FileContent) => void): () => void;
}
```

#### File API Requirements
1. **Primary Implementation (Chrome/Edge):**
   - Use `window.showDirectoryPicker()` for directory selection
   - Implement file reading with `FileSystemFileHandle.getFile()`
   - Cache `lastModified` timestamps to detect changes
   - Handle permission prompts gracefully

2. **Fallback Implementation (Firefox/Safari):**
   - Provide file upload interface for manual snapshots
   - Accept .tar.gz from `systemg export` command
   - Extract and parse uploaded archives
   - Display degraded mode banner

#### Files to Read from ~/.systemg
```
~/.systemg/
├── state/
│   ├── supervisor.pid      # Daemon PID
│   ├── services.state      # Service states (JSON)
│   └── cron.state         # Cron job states (JSON)
├── logs/
│   ├── *.log              # Service logs (text, may be large)
│   └── supervisor.log     # Main daemon log
├── metrics/
│   └── *.json            # Performance metrics
└── config.yaml           # SystemG configuration
```

### PRIORITY 2: Polling System Implementation

Build a robust polling system that reads files efficiently:

#### Single-Flight Polling
```typescript
// services/poller.ts
interface PollerConfig {
  interval: number;        // Default 1000ms
  maxRetries: number;      // Default 3
  backoffMultiplier: number; // Default 1.5
}

class SystemGPoller {
  private inFlight: boolean = false;
  private lastModified: Map<string, number> = new Map();

  async poll(): Promise<StateUpdate> {
    if (this.inFlight) return; // Prevent overlapping reads
    this.inFlight = true;

    try {
      // Read only changed files
      const updates = await this.readChangedFiles();
      return this.processUpdates(updates);
    } finally {
      this.inFlight = false;
    }
  }
}
```

#### Requirements:
- **Never** allow overlapping reads (single-flight pattern)
- Cache file timestamps, skip unchanged files
- Implement exponential backoff on errors (max 30s)
- Handle partial writes gracefully (retry on next poll)
- Memory-efficient for large log files (streaming/chunking)

### PRIORITY 3: Data Sanitization Layer

Implement security layer BEFORE data enters the application:

#### Sanitization Service
```typescript
// services/sanitizer.ts
interface Sanitizer {
  // Remove sensitive environment variables
  sanitizeEnv(env: Record<string, string>): Record<string, string>;

  // Redact secrets from logs
  sanitizeLogs(content: string): string;

  // Clean configuration files
  sanitizeConfig(config: any): any;
}
```

#### Patterns to Redact:
```javascript
const SENSITIVE_PATTERNS = [
  /api[_-]?key/i,
  /password/i,
  /token/i,
  /secret/i,
  /credential/i,
  /private[_-]?key/i,
  // JWT tokens
  /Bearer\s+[A-Za-z0-9\-_=]+\.[A-Za-z0-9\-_=]+\.?[A-Za-z0-9\-_.+/=]*/g,
  // Basic auth in URLs
  /:\/\/([^:]+):([^@]+)@/g
];
```

### PRIORITY 4: Large File Handling

Optimize for performance with large datasets:

#### Streaming Log Reader
```typescript
// services/logReader.ts
class LogReader {
  private offsets: Map<string, number> = new Map();

  async readDelta(file: File, maxBytes = 1048576): Promise<LogDelta> {
    const offset = this.offsets.get(file.name) || 0;

    if (file.size === offset) {
      return { lines: [], truncated: false };
    }

    // Read only new content
    const slice = file.slice(offset, offset + maxBytes);
    const text = await slice.text();

    // Update offset
    this.offsets.set(file.name, offset + slice.size);

    return {
      lines: this.parseLogLines(text),
      truncated: file.size > offset + maxBytes
    };
  }
}
```

#### Requirements:
- Keep per-file offset map for incremental reads
- Cap log payloads at 1MB per read
- Use `File.slice()` for efficient partial reads
- Surface truncation warnings in UI
- Store offsets in IndexedDB for persistence

### PRIORITY 5: Build Configuration

Set up optimized production build:

#### Vite Configuration
```javascript
// vite.config.ts
export default {
  build: {
    target: 'ES2022',
    minify: 'terser',
    rollupOptions: {
      output: {
        manualChunks: {
          vendor: ['react', 'react-dom'],
          ui: ['@chakra-ui/react'],
          store: ['@reduxjs/toolkit']
        }
      }
    }
  },
  optimizeDeps: {
    include: ['react', 'react-dom', '@chakra-ui/react']
  }
}
```

#### Performance Scripts
Create monitoring scripts in `scripts/`:
- `memory-check.js` - Verify <250MB with 1000 processes
- `bundle-size.js` - Ensure <500KB production build
- `perf-test.js` - Validate 60fps rendering

### PRIORITY 6: Testing Infrastructure

Build comprehensive test utilities:

#### Test Helpers
```typescript
// test/utils/mockFileSystem.ts
export function createMockFileSystem() {
  return {
    files: new Map<string, MockFile>(),
    addFile(path: string, content: string, lastModified = Date.now()),
    updateFile(path: string, content: string),
    getFile(path: string): MockFile,
    simulateChange(path: string),
    simulateError(path: string, error: Error)
  };
}
```

#### Test Coverage Requirements:
- File API happy path (Chrome)
- Fallback path (Safari/Firefox)
- Large file handling (>10MB logs)
- Polling with errors and retries
- Sanitization of sensitive data
- Memory leak prevention
- Browser compatibility detection

## Interfaces to Expose

### For FEATURES_DEV (Redux integration):
```typescript
export interface FileSystemAPI {
  initialize(): Promise<void>;
  startPolling(callback: (update: StateUpdate) => void): () => void;
  readSnapshot(): Promise<SystemGSnapshot>;
  exportData(format: 'json' | 'csv'): Promise<Blob>;
}
```

### For UI_DEV (Status indicators):
```typescript
export interface PollingStatus {
  isPolling: boolean;
  lastPollTime: number;
  errorCount: number;
  browserSupported: boolean;
  degradedMode: boolean;
}
```

## Acceptance Criteria

Your infrastructure is complete when:

1. **File Access Works**
   - [ ] Chrome/Edge can select and read ~/.systemg directory
   - [ ] Firefox/Safari show upload option
   - [ ] Permission errors handled gracefully
   - [ ] Files update when changed on disk

2. **Polling is Robust**
   - [ ] Updates every 1 second in steady state
   - [ ] No overlapping reads (single-flight)
   - [ ] Backs off on errors (max 30s)
   - [ ] Memory stable over 1-hour session

3. **Security is Enforced**
   - [ ] No raw passwords in Redux store
   - [ ] Logs sanitized before display
   - [ ] Environment variables masked
   - [ ] Export functions redact secrets

4. **Performance Targets Met**
   - [ ] 1000 processes: <250MB memory
   - [ ] Log tailing: <100ms latency
   - [ ] File changes detected in <2s
   - [ ] No frame drops during polling

5. **Tests Comprehensive**
   - [ ] Unit tests for all services
   - [ ] Integration tests for File API
   - [ ] Performance benchmarks pass
   - [ ] Error scenarios covered

## Implementation Order

1. **Day 1-2:** Browser compatibility detection and File API basics
2. **Day 3-4:** Polling system with single-flight and backoff
3. **Day 5:** Sanitization layer and security
4. **Day 6:** Large file optimization
5. **Day 7:** Test suite and performance validation
6. **Day 8:** Integration with Redux store

## Common Pitfalls to Avoid

1. **Don't poll too aggressively** - Will exhaust browser resources
2. **Don't load entire log files** - Stream/chunk large files
3. **Don't trust file content** - Always sanitize before Redux
4. **Don't ignore partial writes** - SystemG may write mid-poll
5. **Don't leak file handles** - Clean up on unmount
6. **Don't block on permissions** - Handle denials gracefully

## Critical Success Factors

This infrastructure layer is the foundation of the entire application. Without it:
- UI components have no data to display
- Redux store has nothing to manage
- The entire app is non-functional

Your code must be:
- **Bulletproof** - Handle all edge cases
- **Performant** - No memory leaks or CPU spikes
- **Secure** - Never expose sensitive data
- **Testable** - Comprehensive test coverage
- **Well-documented** - Other devs depend on your APIs

Remember: The File API is what makes this a "zero-backend" solution. It's the magic that lets us read SystemG state directly from disk. Make it excellent.

## Artifact-Backed Delivery Requirements
- Do not mark infrastructure tasks complete without concrete service-layer code in `orchestrator-ui/src/services/` and related modules.
- For each task, provide command evidence:
  - `npm run type-check`
  - `npm run test` (services/integration scope or full suite)
  - `npm run build`
- Report-only outputs are not acceptable for implementation work.
- When failures occur, produce remediation patches and rerun verification; do not leave unresolved failed infra tasks at terminal state.
