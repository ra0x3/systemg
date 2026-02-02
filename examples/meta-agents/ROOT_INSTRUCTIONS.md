# Root Agent Instructions

## Your Role
You are the root coordinator agent in a recursive multiplication chain. You orchestrate the entire computation, pass work to recursive agents, and verify the final product.

## Your Task
1. Parse the ordered multiplier list `X=[...]` from your goal
2. Spawn the first recursive agent with the same goal context
3. Wait for the chain to complete bottom-up multiplication
4. Verify the final product against your expectation
5. Persist a summary of the run

## Expected Goal Format
Your goal will be: "Compute chain with X=[2,3,4]"

## Steps to Execute

### 1. Setup
- Create work directory at `/tmp/meta_agents`
- Parse the multipliers and compute the expected product (the product of the list)

### 2. Save Configuration
Create `/tmp/meta_agents/chain_config.json` with:
```json
{
  "x_values": [2, 3, 4],
  "total_depth": 3
}
```

### 3. Spawn First Agent
Run: `sysg spawn --name agent_1 --provider claude --goal "INSTRUCTIONS:RECURSIVE_INSTRUCTIONS.md - Multiply chain for 3 values" -- python3 agent.py`

### 4. Monitor Progress
- Wait for `/tmp/meta_agents/agent_1_output.txt`
- Optionally confirm deeper agents (`agent_2_output.txt`, etc.) appear as the chain unwinds
- Maximum wait time: 90 seconds

### 5. Verify Results
- Read the final product from `agent_1_output.txt`
- Compare against the expected product you computed in step 1
- Gather each agent’s output for reporting

### 6. Save Root Output
Write `/tmp/meta_agents/root_agent_output.txt`:
```json
{
  "agent_name": "root_agent",
  "depth": 0,
  "success": true/false,
  "expected_result": "24",
  "actual_result": "24",
  "x_values": [2, 3, 4],
  "timestamp": "ISO timestamp",
  "agent_outputs": [
    { "agent_name": "agent_1", "result": "24" },
    { "agent_name": "agent_2", "result": "12" },
    { "agent_name": "agent_3", "result": "4" }
  ]
}
```

## Success Criteria
- All agents complete successfully
- Final product matches the expected product
- No timeout occurs

## Error Handling
- Log all errors clearly
- Save output even on failure
- Exit with non-zero code on failure
