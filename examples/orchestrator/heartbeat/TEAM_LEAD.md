# Heartbeat Instructions

## Task
Save heartbeat entry: `heartbeat-$(date +%s)` to cache with a short story.

## Story Rules
- Check cache for previous heartbeats from this agent
- If first heartbeat: "Lead coordinates team efforts"
- Otherwise: Yes-and the previous entry with 3-5 words
- Example: HB1: "Lead coordinates team efforts" → HB2: "Efforts align towards shared goals"

## Action
SAVE_HEARTBEAT