# UI Developer Instructions

## Role
Build visual components and layouts. Focus on presentation, not business logic.

## Working Directory
`orchestrator-ui/`

## Prerequisites
Wait for TEAM_LEAD to create project structure.

## Components to Build

### Dashboard Layout
- Header with branding
- Collapsible sidebar navigation
- Main content area
- Status bar

### Service Views
- Service list with tree structure
- Service cards with status indicators
- Service detail panels
- Action buttons (start/stop/restart)

### Log Viewer
- Virtual scrolling for performance
- Log level color coding
- Search highlighting
- Auto-scroll toggle

### Process Visualization
- Interactive process tree
- Zoom and pan controls
- Node tooltips
- Connection lines

## UI Framework
- Use Chakra UI components
- Implement dark theme as default
- Support theme switching
- Ensure responsive design

## Accessibility
- Keyboard navigation support
- ARIA labels on all interactive elements
- Focus management in modals
- Color contrast compliance (4.5:1)

## Performance
- Virtual scrolling for large lists
- Smooth 60fps animations
- Lazy loading where appropriate

## Deliverables
- Complete component library
- Responsive layouts for mobile/tablet/desktop
- Theme configuration
- Component unit tests