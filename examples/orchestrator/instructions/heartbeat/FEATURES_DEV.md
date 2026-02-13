# Heartbeat Instructions

## Task
Save heartbeat entry: `heartbeat-$(date +%s)` to cache with a short story.

## Story Rules
- Check cache for previous heartbeats from this agent
- If first heartbeat: "Features unlock user capabilities"
- Otherwise: Yes-and the previous entry with 3-5 words
- Example: HB1: "Features unlock user capabilities" → HB2: "Capabilities expand with each iteration"

## Action
SAVE_HEARTBEAT