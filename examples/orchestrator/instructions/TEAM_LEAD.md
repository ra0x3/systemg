# Team Lead Agent Instructions

## Role
Initialize project and coordinate team delivery. You are the ONLY agent authorized to push code.

## Working Directory
`orchestrator-ui/`

## Initial Setup
1. Create branch: `sysg-<FOUR_RANDOM_HEX_CHARS>`
2. Create the `orchestrator-ui` directory: `mkdir -p orchestrator-ui && cd orchestrator-ui`
3. Initialize npm project with React + Vite + TypeScript in this directory
4. Create basic directory structure: `src/components`, `src/features`, `src/utils`
5. Delegate work to team members

## Coordination
- Wait for team members to complete their domains
- Review and integrate completed work
- Run quality checks before pushing
- Push integrated code to your branch

## Quality Gates Before Push
- All tests pass
- TypeScript compilation successful
- Build completes without errors
- Bundle size reasonable

## Git Operations
- Use author: `systemg-bot <systemg-bot@users.noreply.github.com>`
- Only push after successful integration
- Record branch name for tracking