# Recursive Agent Instructions

## Your Role
You are a recursive multiplication agent in a chain. You delegate work to the next agent (if any), wait for its product, and then multiply by your own value as the recursion unwinds.

## Your Task
1. Look up the multiplier assigned to your depth from `chain_config.json`
2. Spawn the next agent if the chain continues deeper
3. Wait for the child’s result (or treat it as 1 if you are the base case)
4. Multiply your multiplier by the child result
5. Save your result to disk

## Expected Goal Format
Your goal will be: "Multiply chain for 3 values"

## Steps to Execute

### 1. Identify Your Multiplier
- Read `/tmp/meta_agents/chain_config.json`
- Determine the multiplier at index `depth - 1`

### 2. Spawn Child (If Needed)
- If `depth < total_depth`, spawn the next agent using:
  `sysg spawn --name agent_{depth+1} --provider claude --goal "INSTRUCTIONS:RECURSIVE_INSTRUCTIONS.md - Multiply chain for remaining values (depth {depth+1})" -- python3 agent.py`
- Wait for `/tmp/meta_agents/agent_{depth+1}_output.txt`

### 3. Multiply
- If you spawned a child, set `child_result` to the value from its output
- Otherwise (base case), set `child_result = 1`
- Compute `result = multiplier * child_result`

### 4. Save Output
Write `/tmp/meta_agents/agent_{depth}_output.txt`:
```json
{
  "agent_name": "agent_{depth}",
  "depth": depth,
  "multiplier": multiplier,
  "child_result": child_result,
  "result": result,
  "timestamp": "ISO timestamp"
}
```

## Chain Termination
- Do not spawn a child when `depth == total_depth`
- Always wait for the child result before computing your own

## Error Handling
- Log any failure (missing config, spawn error, invalid child result)
- Still write your output file with the best information available
- Exit with non-zero status on failure
