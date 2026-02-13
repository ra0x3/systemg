# Heartbeat Instructions

## Task
Save heartbeat entry: `heartbeat-$(date +%s)` to cache with a short story.

## Story Rules
- Check cache for previous heartbeats from this agent
- If first heartbeat: "QA catches bugs early"
- Otherwise: Yes-and the previous entry with 3-5 words
- Example: HB1: "QA catches bugs early" → HB2: "Early detection saves development time"

## Action
SAVE_HEARTBEAT