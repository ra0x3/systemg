# Heartbeat Instructions

## Task
Save heartbeat entry: `heartbeat-$(date +%s)` to cache with a short story.

## Story Rules
- Check cache for previous heartbeats from this agent
- If first heartbeat: "Owner sets the vision"
- Otherwise: Yes-and the previous entry with 3-5 words
- Example: HB1: "Owner sets the vision" → HB2: "Vision guides team forward boldly"

## Action
SAVE_HEARTBEAT