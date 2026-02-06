# Core Infrastructure Developer Instructions

## Role
You own the foundation layer: Redux store, polling system, data sanitization, and browser compatibility.

## Working Directory
`systemg/ui`

## Core Responsibilities

### 1. Redux Store Setup
Create a sanitized, type-safe state management system:

```typescript
// src/store/index.ts
interface SystemGState {
  services: ServiceState[]
  logs: LogEntry[]
  metrics: MetricsData
  config: ConfigData
  polling: PollingState
}
```

Key requirements:
- Sanitize all data (no secrets, no sensitive paths)
- Implement proper TypeScript types
- Use RTK Query for async operations
- Handle partial/failed updates gracefully

### 2. Polling System
Implement robust file polling with exponential backoff:

```typescript
// src/utils/polling.ts
- readJsonSnapshot(path: string): Promise<any>
- readLogDelta(path: string, lastOffset: number): Promise<LogDelta>
- pollWithBackoff(fn: Function, interval: number): Subscription
```

Requirements:
- Start at 1s intervals, back off to 30s on errors
- Handle file not found gracefully
- Detect stale data (>60s old)
- Clean up subscriptions on unmount

### 3. Browser Compatibility Layer
Create fallback for browsers without File System Access API:

```typescript
// src/utils/browser-compat.ts
- detectFileAPISupport(): boolean
- createManualUploadFallback(): UploadHandler
- showCompatibilityWarning(): void
```

Supported browsers:
- Chrome 86+ (full support)
- Safari (manual upload)
- Firefox (manual upload)

### 4. Data Sanitization
Implement sanitization for all external data:

```typescript
// src/utils/sanitize.ts
- sanitizePath(path: string): string  // Remove user home, secrets
- sanitizeEnv(env: Record<string, string>): Record<string, string>
- sanitizeConfig(config: any): any
```

Rules:
- Replace `/home/username` with `~`
- Remove tokens/keys/passwords
- Truncate large values (>10KB)

### 5. Error Handling
Comprehensive error boundary and recovery:

```typescript
// src/utils/errors.ts
- ErrorBoundary component
- Retry logic with exponential backoff
- User-friendly error messages
- Error reporting to telemetry
```

## Testing Requirements
Write comprehensive tests for:
- Redux reducers and selectors
- Polling lifecycle and cleanup
- Browser detection logic
- Sanitization rules
- Error recovery flows

Minimum coverage: 90% for utils, 80% for hooks

## Performance Targets
- Initial load: <100ms
- Polling overhead: <5% CPU
- Memory usage: <50MB baseline
- Handle 1000+ services without degradation

## Deliverables
- [ ] Complete Redux store with all slices
- [ ] Polling system with backoff and cleanup
- [ ] Browser compatibility layer
- [ ] Data sanitization utilities
- [ ] Comprehensive test suite
- [ ] Performance benchmarks documented