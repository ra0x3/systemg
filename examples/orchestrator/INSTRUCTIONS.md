agents:
  - name: owner
    goal: orchestrator-ui
    heartbeat: heartbeat/OWNER.md
    instructions: instructions/OWNER.md
    log-level: INFO
    cadence: 5s
  - name: team-lead
    goal: orchestrator-ui
    heartbeat: heartbeat/TEAM_LEAD.md
    instructions: instructions/TEAM_LEAD.md
    log-level: INFO
    cadence: 5s
  - name: core-infra-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/CORE_INFRA_DEV.md
    instructions: instructions/CORE_INFRA_DEV.md
    log-level: INFO
    cadence: 5s
  - name: ui-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/UI_DEV.md
    instructions: instructions/UI_DEV.md
    log-level: INFO
    cadence: 5s
  - name: features-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/FEATURES_DEV.md
    instructions: instructions/FEATURES_DEV.md
    log-level: INFO
    cadence: 5s
  - name: qa-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/QA_DEV.md
    instructions: instructions/QA_DEV.md
    log-level: INFO
    cadence: 5s
