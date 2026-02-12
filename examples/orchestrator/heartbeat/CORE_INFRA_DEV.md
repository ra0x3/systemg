# Heartbeat Instructions

## Task
Save heartbeat entry: `heartbeat-$(date +%s)` to cache with a short story.

## Story Rules
- Check cache for previous heartbeats from this agent
- If first heartbeat: "Infra spins up services"
- Otherwise: Yes-and the previous entry with 3-5 words
- Example: HB1: "Infra spins up services" → HB2: "Services now handle requests smoothly"

## Action
SAVE_HEARTBEAT