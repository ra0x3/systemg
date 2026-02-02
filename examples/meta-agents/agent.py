#!/usr/bin/env python3
"""
Universal meta-agent that reads instructions based on goal.
This agent adapts its behavior based on instruction files passed via --goal.
"""

import json
import logging
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import List, Optional, Tuple

# Configure logging with stdout handler
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s',
    stream=sys.stdout,
    force=True
)
logger = logging.getLogger("meta_agent")

WORK_DIR = Path(os.environ.get("WORK_DIR", "/tmp/meta_agents"))
FINAL_WAIT_SECONDS = int(os.environ.get("META_AGENT_WAIT_SECONDS", "90"))
POLL_INTERVAL_SECONDS = 1

def setup_work_dir():
    """Create work directory if it doesn't exist."""
    WORK_DIR.mkdir(parents=True, exist_ok=True)

def get_agent_info():
    """Get current agent information from environment."""
    depth = int(os.environ.get("SPAWN_DEPTH", "0"))
    parent_pid = os.environ.get("SPAWN_PARENT_PID", "unknown")
    goal = os.environ.get("AGENT_GOAL", "")
    agent_name = os.environ.get("AGENT_NAME", f"agent_{depth}")

    return {
        "depth": depth,
        "parent_pid": parent_pid,
        "goal": goal,
        "name": agent_name
    }

def read_instructions(goal: str) -> Tuple[str, str]:
    """Parse goal to determine the instruction file and task payload."""
    if goal:
        match = re.search(r"INSTRUCTIONS:\s*([^\s-]+)", goal)
        if match:
            instruction_file = match.group(1).strip()

            # Task payload is everything after the first ' - '
            payload = goal.split(" - ", 1)
            task = payload[1].strip() if len(payload) == 2 else goal
            return instruction_file, task

    depth = int(os.environ.get("SPAWN_DEPTH", "0"))
    default_instruction = "ROOT_INSTRUCTIONS.md" if depth == 0 else "RECURSIVE_INSTRUCTIONS.md"
    return default_instruction, goal

def load_instructions(instruction_file):
    """Load and return instruction content."""
    script_dir = Path(__file__).parent
    instruction_path = script_dir / instruction_file

    if not instruction_path.exists():
        logger.error(f"Instruction file not found: {instruction_path}")
        return None

    with open(instruction_path, 'r') as f:
        return f.read()


def parse_chain_parameters(task: str) -> List[int]:
    """Extract the ordered multiplier list from the task description."""
    x_match = re.search(r"X=\[([^\]]+)\]", task)
    if not x_match:
        raise ValueError("Missing X array in goal task")

    try:
        x_values = [int(piece.strip()) for piece in x_match.group(1).split(',') if piece.strip()]
    except ValueError as err:
        raise ValueError(f"Invalid X value in goal: {err}") from err

    if not x_values:
        raise ValueError("No X values provided in goal task")
    return x_values


def normalize_result(value: int) -> str:
    """Convert numeric results to a normalized string for comparisons and logging."""
    return str(value)


def clear_previous_outputs():
    """Remove stale agent output files to avoid reading previous run artifacts."""
    for output_file in WORK_DIR.glob("agent_*_output.txt"):
        try:
            output_file.unlink()
        except OSError:
            pass


def compute_expected_product(values: List[int]) -> int:
    """Multiply all values to determine the expected result."""
    product = 1
    for value in values:
        product *= value
    return product


def wait_for_agent_output(depth: int, timeout_seconds: int) -> Optional[dict]:
    """Poll for a specific agent's output file and return its JSON content."""
    target_path = WORK_DIR / f"agent_{depth}_output.txt"
    deadline = time.time() + timeout_seconds
    while time.time() < deadline:
        if target_path.exists():
            try:
                with open(target_path, "r") as handle:
                    return json.load(handle)
            except json.JSONDecodeError:
                time.sleep(POLL_INTERVAL_SECONDS)
                continue
        time.sleep(POLL_INTERVAL_SECONDS)
    return None


def record_root_summary(
    success: bool,
    x_values: List[int],
    expected_result: int,
    actual_result: int,
    *,
    agent_outputs: List[dict],
    error: Optional[str] = None,
):
    """Persist the root agent's summary to disk."""
    root_output = {
        "agent_name": "root_agent",
        "depth": 0,
        "success": bool(success),
        "x_values": x_values,
        "expected_result": normalize_result(expected_result),
        "actual_result": normalize_result(actual_result),
        "timestamp": datetime.now().isoformat(),
    }

    root_output["agent_outputs"] = agent_outputs
    if error:
        root_output["error"] = error

    root_file = WORK_DIR / "root_agent_output.txt"
    with open(root_file, "w") as handle:
        json.dump(root_output, handle, indent=2)
    logger.info(f"[ROOT] Saved output to {root_file}")

def execute_root_logic(task: str) -> bool:
    """Coordinate the bottom-up multiplication chain."""
    logger.info("[ROOT] Executing root agent logic")

    try:
        x_values = parse_chain_parameters(task)
    except ValueError as err:
        logger.error(f"[ROOT] {err}")
        record_root_summary(
            False,
            [],
            1,
            0,
            agent_outputs=[],
            error=str(err),
        )
        return False

    logger.info(f"[ROOT] Multipliers: {x_values}")

    clear_previous_outputs()

    expected_result = compute_expected_product(x_values)
    logger.info(f"[ROOT] Expected final product: {expected_result}")

    config_payload = {
        "x_values": x_values,
        "total_depth": len(x_values),
        "created_at": datetime.now().isoformat(),
    }

    config_file = WORK_DIR / "chain_config.json"
    with open(config_file, "w") as handle:
        json.dump(config_payload, handle, indent=2)
    logger.info(f"[ROOT] Saved chain configuration to {config_file}")

    first_goal = (
        "INSTRUCTIONS:RECURSIVE_INSTRUCTIONS.md - "
        f"Multiply chain for {len(x_values)} values"
    )

    logger.info(f"[ROOT] Dispatching first child with goal: {first_goal}")
    pid = spawn_agent("agent_1", first_goal, 1)
    if not pid:
        record_root_summary(
            False,
            x_values,
            expected_result,
            0,
            agent_outputs=[],
            error="Failed to spawn first child agent",
        )
        return False

    logger.info(f"[ROOT] Spawned agent_1 with PID {pid}")

    logger.info(
        f"[ROOT] Waiting for agent_1 result (timeout {FINAL_WAIT_SECONDS}s)"
    )
    agent_one_payload = wait_for_agent_output(1, FINAL_WAIT_SECONDS)
    if not agent_one_payload:
        logger.error("[ROOT] agent_1 did not complete in time")
        record_root_summary(
            False,
            x_values,
            expected_result,
            0,
            agent_outputs=[],
            error="agent_1 did not complete in time",
        )
        return False

    actual_raw_result = agent_one_payload.get("result")
    if actual_raw_result is None:
        logger.error("[ROOT] agent_1 result missing")
        record_root_summary(
            False,
            x_values,
            expected_result,
            0,
            agent_outputs=[agent_one_payload],
            error="agent_1 result missing",
        )
        return False

    ordered_outputs: List[dict] = []
    for depth_idx in range(1, len(x_values) + 1):
        if depth_idx == 1:
            payload = agent_one_payload
        else:
            payload = wait_for_agent_output(depth_idx, 2)
        if payload is None:
            logger.warning(f"[ROOT] Missing output for agent_{depth_idx}")
            continue
        ordered_outputs.append(payload)

    try:
        actual_result = int(actual_raw_result)
    except (TypeError, ValueError):
        logger.error(
            f"[ROOT] agent_1 result is not an integer: {actual_raw_result}"
        )
        record_root_summary(
            False,
            x_values,
            expected_result,
            0,
            agent_outputs=ordered_outputs,
            error="agent_1 result not an integer",
        )
        return False

    logger.info(f"[ROOT] Final result reported: {actual_result}")

    success = actual_result == expected_result
    if success:
        logger.info("[ROOT] Success: multiplication chain completed")
    else:
        logger.error(
            "[ROOT] Final product mismatch "
            f"(expected {expected_result}, got {actual_result})"
        )

    record_root_summary(
        success,
        x_values,
        expected_result,
        actual_result,
        agent_outputs=ordered_outputs,
    )

    return success

def execute_recursive_logic(task: str, depth: int) -> bool:
    """Execute one level of the multiplication chain."""
    logger.info(f"[{depth}] Executing recursive agent logic")

    config_path = WORK_DIR / "chain_config.json"
    try:
        with open(config_path, "r") as handle:
            config = json.load(handle)
    except FileNotFoundError:
        logger.error(f"[{depth}] Missing chain_config.json")
        return False

    x_values = config.get("x_values", [])
    total_depth = config.get("total_depth", len(x_values))

    if depth < 1 or depth > len(x_values):
        logger.error(f"[{depth}] Depth out of range for provided values")
        return False

    current_multiplier = x_values[depth - 1]
    logger.info(f"[{depth}] Current multiplier: {current_multiplier}")

    child_result = 1
    if depth < total_depth:
        child_name = f"agent_{depth + 1}"
        child_goal = (
            "INSTRUCTIONS:RECURSIVE_INSTRUCTIONS.md - "
            f"Multiply chain for remaining values (depth {depth + 1})"
        )
        logger.info(f"[{depth}] Spawning child {child_name} -> goal: {child_goal}")
        child_pid = spawn_agent(child_name, child_goal, depth + 1)
        if not child_pid:
            logger.error(f"[{depth}] Failed to spawn child agent {child_name}")
            return False

        logger.info(f"[{depth}] Waiting for child result from depth {depth + 1}")
        child_payload = wait_for_agent_output(depth + 1, FINAL_WAIT_SECONDS)
        if not child_payload:
            logger.error(f"[{depth}] Child agent {child_name} timed out")
            return False

        try:
            child_result = int(child_payload.get("result", 0))
        except (TypeError, ValueError):
            logger.error(f"[{depth}] Child result is not an integer: {child_payload}")
            return False

        logger.info(f"[{depth}] Child result: {child_result}")
    else:
        logger.info(f"[{depth}] Base case reached; using child result = 1")

    result = current_multiplier * child_result
    logger.info(f"[{depth}] Computed result: {result}")

    output = {
        "agent_name": f"agent_{depth}",
        "depth": depth,
        "multiplier": current_multiplier,
        "child_result": child_result,
        "result": result,
        "timestamp": datetime.now().isoformat(),
    }

    output_file = WORK_DIR / f"agent_{depth}_output.txt"
    with open(output_file, "w") as handle:
        json.dump(output, handle, indent=2)
    logger.info(f"[{depth}] Saved output to {output_file}")

    # Keep agent alive briefly to show full spawn tree in status
    if depth > 0:
        logger.info(f"[{depth}] Keeping alive briefly for status visibility...")
        time.sleep(8 - min(depth * 2, 6))  # Stagger the exit times

    return True

def spawn_agent(name: str, goal: str, depth: int) -> Optional[int]:
    """Spawn a child agent via `sysg` or fall back to direct execution."""
    logger.info(f"[SPAWN] Spawning {name} with goal: {goal}")

    script_dir = Path(__file__).resolve().parent
    # Use workspace release binary
    sysg_path = script_dir / ".." / ".." / "target" / "release" / "sysg"

    if not sysg_path.exists():
        # Fall back to system sysg if workspace binary doesn't exist
        sysg_path_str = shutil.which("sysg")
        if sysg_path_str:
            sysg_path = Path(sysg_path_str)
        else:
            sysg_path = None

    if sysg_path is None or not sysg_path.exists():
        logger.warning("[SPAWN] `sysg` not found; running child directly")
        env = os.environ.copy()
        env["SPAWN_DEPTH"] = str(depth)
        env["AGENT_NAME"] = name
        env["AGENT_GOAL"] = goal
        process = subprocess.Popen(
            ["python3", str(script_dir / "agent.py")],
            env=env,
        )
        logger.info(f"[SPAWN] Directly launched child process PID {process.pid}")
        return process.pid

    # Get current process PID to pass as parent
    current_pid = os.getpid()

    command = [
        str(sysg_path),
        "spawn",
        "--name",
        name,
        "--parent-pid",
        str(current_pid),
        "--provider",
        "claude",
        "--goal",
        goal,
        "--",
        "python3",
        str(script_dir / "agent.py"),
    ]

    env = os.environ.copy()

    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )

    try:
        stdout, stderr = process.communicate(timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()
        logger.error(f"[SPAWN] sysg spawn command timed out for {name}")
        return None

    stdout = (stdout or "").strip()
    stderr = (stderr or "").strip()

    if process.returncode != 0:
        logger.error(f"[SPAWN] Failed to spawn {name} (exit code {process.returncode})")
        if stdout:
            logger.info(f"[SPAWN] stdout: {stdout}")
        if stderr:
            logger.info(f"[SPAWN] stderr: {stderr}")
        return None

    pid_match = re.search(r"(\d+)", stdout)
    if not pid_match:
        logger.error(f"[SPAWN] Could not parse PID from output: '{stdout}'")
        if stderr:
            logger.info(f"[SPAWN] stderr: {stderr}")
        return None

    pid = int(pid_match.group(1))
    logger.info(f"[SPAWN] Spawned {name} with PID: {pid}")
    if stderr:
        logger.info(f"[SPAWN] stderr: {stderr}")
    return pid

def main():
    """Main entry point for universal agent."""
    setup_work_dir()

    # Get agent info
    agent_info = get_agent_info()
    depth = agent_info["depth"]
    goal = agent_info["goal"]

    logger.info(f"[AGENT] Universal agent starting...")
    logger.info(f"[AGENT] Name: {agent_info['name']}")
    logger.info(f"[AGENT] Depth: {depth}")
    logger.info(f"[AGENT] Parent PID: {agent_info['parent_pid']}")
    logger.info(f"[AGENT] Goal: {goal}")

    # Read and parse instructions
    instruction_file, task = read_instructions(goal)
    logger.info(f"[AGENT] Using instructions: {instruction_file}")
    logger.info(f"[AGENT] Task: {task}")

    instructions = load_instructions(instruction_file)
    if not instructions:
        logger.error("[AGENT] Could not load instructions")
        sys.exit(1)

    # Execute based on role/instructions
    if "ROOT" in instruction_file.upper() or depth == 0:
        success = execute_root_logic(task)
    else:
        success = execute_recursive_logic(task, depth)

    if success:
        logger.info(f"[AGENT] {agent_info['name']} completed successfully")
        if depth == 0:
            logger.info(f"[AGENT] Root summary saved at {WORK_DIR / 'root_agent_output.txt'}")
            # Keep root agent running briefly to show sub-agents in status
            logger.info("[AGENT] Keeping process alive briefly for status visibility...")
            time.sleep(10)
    else:
        logger.info(f"[AGENT] {agent_info['name']} failed")
        sys.exit(1)

if __name__ == "__main__":
    main()
