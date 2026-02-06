# Features Developer Instructions

## Role
You implement advanced features: search, filtering, telemetry, exports, and configuration management.

## Working Directory
`systemg/ui`

## Core Responsibilities

### 1. Global Search System
Implement powerful search across all data:

```typescript
// src/features/search/
- GlobalSearch: Omnisearch bar with shortcuts (Cmd+K)
- SearchIndex: Client-side indexing with Fuse.js
- SearchResults: Grouped by type (services, logs, config)
- SearchHighlight: Highlight matches in context
```

Requirements:
- Instant results as you type
- Fuzzy matching support
- Search history persistence
- Keyboard navigation in results
- Search across: services, logs, configs, metrics

### 2. Advanced Filtering
Multi-dimensional filtering system:

```typescript
// src/features/filters/
- FilterBar: Visual filter builder
- FilterChips: Active filter display
- FilterPresets: Save/load filter combinations
- FilterSync: URL-based filter state
```

Filter dimensions:
- Service status (running, stopped, error)
- Time ranges (custom or presets)
- Log levels (debug, info, warn, error)
- Resource usage thresholds
- Custom regex patterns

### 3. Configuration Viewer
YAML configuration display and editing:

```typescript
// src/features/config/
- ConfigViewer: Syntax highlighted YAML
- ConfigDiff: Show changes over time
- ConfigValidator: Real-time validation
- ConfigSearch: Find in configuration
```

Features:
- Monaco Editor integration
- Schema validation
- Folding/expanding sections
- Copy config sections
- Safe editing mode (no direct saves)

### 4. Cron Scheduler Dashboard
Visualize and manage scheduled tasks:

```typescript
// src/features/cron/
- CronDashboard: Calendar view of jobs
- CronTimeline: Upcoming executions
- CronHistory: Past run results
- CronEditor: Visual cron expression builder
```

Display:
- Next 24 hours timeline
- Success/failure indicators
- Execution duration trends
- Overlap warnings
- Manual trigger button

### 5. Export System
Export data in multiple formats:

```typescript
// src/features/export/
- ExportModal: Format and range selection
- ExportFormats: CSV, JSON, PDF report
- ExportScheduler: Automated exports
- ExportTemplates: Predefined export configs
```

Capabilities:
- Export filtered data only
- Custom date ranges
- Include/exclude columns
- Size limits (10MB max)
- Progress indication for large exports

### 6. Telemetry Dashboard
Real-time metrics and analytics:

```typescript
// src/features/telemetry/
- MetricsGrid: Key performance indicators
- MetricsCharts: Time series visualizations
- MetricsAlerts: Threshold-based warnings
- MetricsExport: Historical data download
```

Metrics to track:
- Polling performance (requests/sec, latency)
- UI responsiveness (frame rate, input lag)
- Memory usage trends
- Token consumption (if applicable)
- Error rates and types

### 7. Token Usage Monitor
Track and optimize LLM token usage:

```typescript
// src/features/tokens/
- TokenCounter: Real-time usage display
- TokenBudget: Daily/monthly limits
- TokenOptimizer: Suggestions for reduction
- TokenReports: Usage breakdown by agent
```

Features:
- Live token counting
- Budget alerts at 80%, 90%, 100%
- Historical usage graphs
- Per-agent breakdown
- Optimization recommendations

### 8. Keyboard Shortcuts
Comprehensive keyboard control:

```typescript
// src/features/shortcuts/
- ShortcutManager: Global key handler
- ShortcutOverlay: Show all shortcuts (?)
- ShortcutCustomizer: User-defined shortcuts
```

Default shortcuts:
- `Cmd+K`: Global search
- `J/K`: Navigate up/down
- `Enter`: Select/expand
- `Esc`: Close/cancel
- `Cmd+E`: Export
- `Cmd+F`: Filter
- `R`: Refresh

## Integration Requirements
- All features integrate with Redux store
- URL state synchronization for sharing
- Proper loading and error states
- Progressive enhancement approach
- Mobile-responsive implementations

## Performance Targets
- Search results: <100ms
- Filter application: <50ms
- Export generation: <5s for 10MB
- Config rendering: <200ms
- Zero blocking UI operations

## Testing Requirements
- Unit tests for all utilities
- Integration tests for complex flows
- E2E tests for critical paths
- Performance benchmarks
- Memory leak detection

## Deliverables
- [ ] Global search with indexing
- [ ] Advanced filtering system
- [ ] Configuration viewer
- [ ] Cron scheduler dashboard
- [ ] Multi-format export
- [ ] Telemetry dashboard
- [ ] Token usage monitoring
- [ ] Keyboard shortcut system
- [ ] Feature documentation