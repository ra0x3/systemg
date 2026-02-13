# Features Developer Instructions

## Role
Implement core business logic and data management. Focus on functionality, not UI.

## Working Directory
`orchestrator-ui/`

## Prerequisites
Wait for TEAM_LEAD to create project structure.

## Core Features to Implement

### State Management
- Redux store configuration
- Service state management
- Log data handling
- Metrics processing

### Data Polling System
- File-based snapshot reading
- Polling with exponential backoff
- Stale data detection
- Error recovery

### Search and Filtering
- Client-side search indexing
- Multi-field filtering logic
- Filter persistence in URL
- Search result ranking

### Export System
- Data serialization to CSV/JSON
- Filtered data export
- Batch export handling

### Configuration Management
- YAML config parsing
- Configuration validation
- Settings persistence

## Data Handling
- Sanitize all sensitive information
- Handle large datasets efficiently
- Implement proper error boundaries

## Performance Requirements
- Search results <100ms
- Filter application <50ms
- Export generation <5s for 10MB

## Deliverables
- Core features implemented with proper types
- Unit tests for all business logic
- Data handling utilities documented