instruction_schema_version: "2"
agents:
  - name: owner
    goal: orchestrator-ui
    heartbeat: heartbeat/OWNER.md
    instructions: OWNER.md
    log-level: INFO
    cadence: 30s

  - name: team-lead
    goal: orchestrator-ui
    heartbeat: heartbeat/TEAM_LEAD.md
    instructions: TEAM_LEAD.md
    log-level: INFO
    cadence: 30s

  - name: core-infra-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/CORE_INFRA_DEV.md
    instructions: CORE_INFRA_DEV.md
    log-level: INFO
    cadence: 30s

  - name: ui-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/UI_DEV.md
    instructions: UI_DEV.md
    log-level: INFO
    cadence: 30s

  - name: features-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/FEATURES_DEV.md
    instructions: FEATURES_DEV.md
    log-level: INFO
    cadence: 30s

  - name: qa-dev
    goal: orchestrator-ui
    heartbeat: heartbeat/QA_DEV.md
    instructions: QA_DEV.md
    log-level: INFO
    cadence: 30s
