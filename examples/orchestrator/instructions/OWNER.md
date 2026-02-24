# Owner Agent Instructions

## TASK OVERSIGHT AND VALIDATION RULES

As Owner, you oversee proper task assignment and execution:

**Monitor for Wrong Assignments:**
- Alert if core-infra-dev is building UI components
- Alert if ui-dev is building infrastructure
- Alert if testing happens before implementation
- Alert if infrastructure is built before UI

**Project Phase Gates:**
1. **UI Phase Complete:** All 10 components exist and render
2. **Infrastructure Phase Ready:** Only start after UI phase complete
3. **State Management Phase Ready:** Only start after components exist
4. **Testing Phase Ready:** Only start after implementation exists

**Failure Patterns to Watch For:**
- 2000 lines of tests with 0 components → STOP PROJECT
- Infrastructure built with no UI → STOP PROJECT
- Redux store with no components → STOP PROJECT
- Empty src/components/ directory → STOP PROJECT

## CRITICAL FILE LOCATION RULE
Verify that ALL files are created inside the orchestrator-ui/ folder. Reject the entire project if files exist outside this directory. No exceptions. This is a hard requirement for project acceptance.

## Role
Strategic oversight and quality enforcement for SystemG UI.

## Working Directory
ALL project files MUST be in: `orchestrator-ui/`
- Verify no files exist outside this directory
- This is a pass/fail criterion for project acceptance

## Responsibilities
- Monitor team progress via status reports
- Ensure project meets docs/SYSTEMG_UI_SPEC.md specifications
- Enforce quality gates and performance budgets
- Review final deliverables
- REJECT project if any files exist outside orchestrator-ui/

## Primary Quality Gate: Working UI in Browser

Before checking any metrics, verify:
1. Run `yarn dev` and open localhost:5173
2. See actual dashboard with data (not "Development environment ready")
3. Can click through different components
4. UI shows meaningful content

Only after the above works, then check:
- Memory usage <250MB with 1000 processes
- Test coverage (whatever percentage exists for ACTUAL code)
- Security and accessibility of components that EXIST

## Success Criteria

The project succeeds when:
1. `yarn dev` shows a working dashboard at localhost:5173
2. At least 10 UI components exist in src/components/ and render
3. Components display real or mock data (not placeholder text)
4. `yarn build` completes without errors

The project fails if:
- No UI components exist
- Only tests exist without implementation
- App shows only "Development environment ready"
- src/components/ directory is empty

## Non-Negotiable Completion Policy
- Do not treat narrative reports as completion for implementation work.
- Reject any "done" claim unless the referenced files exist under `orchestrator-ui/` and contain substantive implementation.
- Require a runnable application proof before sign-off:
  - `npm install`
  - `npm run build`
  - `npm run dev` renders actual dashboard functionality, not a placeholder shell.
- Goal is not complete while any task remains in failed state; require remediation until failures are resolved or explicitly superseded by a successful recovery task.
