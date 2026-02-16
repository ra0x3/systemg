# Orchestrator Instructions

## Runtime Prerequisites

- `sysg` (systemg CLI) must be installed and available on PATH.
- `porki` must be installed from PyPI and available on PATH.
- Redis must be running and reachable at `redis://127.0.0.1:6379`.

## Global Output Policy

All report outputs must be written under the `reports/` directory in this
orchestrator example.

## Agent Configuration

```yaml
agents:
  - name: owner
    goal: orchestrator-ui
    heartbeat: instructions/heartbeat/OWNER.md
    instructions: instructions/OWNER.md
    log-level: INFO
    cadence: 30s

  - name: team-lead
    goal: orchestrator-ui
    heartbeat: instructions/heartbeat/TEAM_LEAD.md
    instructions: instructions/TEAM_LEAD.md
    log-level: INFO
    cadence: 30s

  - name: core-infra-dev
    goal: orchestrator-ui
    heartbeat: instructions/heartbeat/CORE_INFRA_DEV.md
    instructions: instructions/CORE_INFRA_DEV.md
    log-level: INFO
    cadence: 30s

  - name: ui-dev
    goal: orchestrator-ui
    heartbeat: instructions/heartbeat/UI_DEV.md
    instructions: instructions/UI_DEV.md
    log-level: INFO
    cadence: 30s

  - name: features-dev
    goal: orchestrator-ui
    heartbeat: instructions/heartbeat/FEATURES_DEV.md
    instructions: instructions/FEATURES_DEV.md
    log-level: INFO
    cadence: 30s

  - name: qa-dev
    goal: orchestrator-ui
    heartbeat: instructions/heartbeat/QA_DEV.md
    instructions: instructions/QA_DEV.md
    log-level: INFO
    cadence: 30s
```
