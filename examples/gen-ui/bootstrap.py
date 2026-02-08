#!/usr/bin/env python3
"""
Minimal Bootstrap - Let the LLM write everything.

This script does ONE thing: tells Claude to write its own root coordinator,
which will then write its own team member scripts.
"""

import subprocess
import sys
import os

def bootstrap():
    """Tell Claude to create the entire agent system from scratch."""

    print("🚀 Bootstrap: Asking Claude to create the root coordinator...")

    # Read the high-level instructions if they exist
    instructions_file = "INSTRUCTIONS.md"
    instructions = ""
    if os.path.exists(instructions_file):
        with open(instructions_file, "r") as f:
            instructions = f.read()

    prompt = f"""Create a Python script called 'root_coordinator.py' that:

1. Acts as the ROOT/OWNER of a software project
2. Reads the project requirements from INSTRUCTIONS.md
3. Decides what team members (agents) are needed
4. Writes Python scripts for each team member
5. Uses 'sysg spawn' to launch each team member
6. Monitors progress by reading their console output
7. Coordinates the team to build the project

Project Requirements:
{instructions}

Important guidelines:
- The root coordinator should write actual Python scripts for team members
- Each team member script should use subprocess.run() to call Claude for their work
- Team members should print clear progress updates to stdout
- Use 'sysg spawn --name <name> -- python3 <script.py>' to launch team members
- Don't use sysg inspect repeatedly - just let processes stream output
- Keep it simple and focused on getting work done

Write the complete root_coordinator.py script now."""

    # Have Claude write the root coordinator
    result = subprocess.run(
        ["claude", "--dangerously-skip-permissions", "-p", prompt],
        capture_output=False  # Stream output so we see progress
    )

    if result.returncode != 0:
        print("❌ Failed to create root coordinator")
        return False

    print("\n✅ Root coordinator created. Now launching it...\n")

    # Now spawn the root coordinator
    result = subprocess.run(
        ["sysg", "spawn", "--name", "root_coordinator", "--", sys.executable, "root_coordinator.py"],
        capture_output=False  # Let output stream to console
    )

    return result.returncode == 0

if __name__ == "__main__":
    success = bootstrap()
    sys.exit(0 if success else 1)