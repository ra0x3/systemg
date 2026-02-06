#!/usr/bin/env python3
"""
SystemG UI Agent Framework.

This module provides an intelligent agent system for coordinating
software development tasks. Agents can execute work via Claude,
monitor team status, and coordinate through signal files.
"""

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass, asdict
from datetime import datetime
from enum import Enum, auto
from pathlib import Path
from typing import Dict, List, Optional, Any, Union


class AgentRole(Enum):
    """Enumeration of available agent roles."""

    OWNER = "owner"
    TEAM_LEAD = "team_lead"
    CORE_INFRA_DEV = "core_infra_dev"
    UI_DEV = "ui_dev"
    FEATURES_DEV = "features_dev"
    QA_DEV = "qa_dev"


class AgentStatus(Enum):
    """Enumeration of agent status states."""

    IDLE = "idle"
    WAITING = "waiting"
    WORKING = "working"
    DONE = "done"
    FAILED = "failed"
    BLOCKED = "blocked"


class WorkPhase(Enum):
    """Enumeration of work phases."""

    INITIALIZING = auto()
    PROJECT_SETUP = auto()
    DEVELOPMENT = auto()
    INTEGRATION = auto()
    TESTING = auto()
    REVIEW = auto()
    DEPLOYMENT = auto()
    COMPLETE = auto()


@dataclass
class AgentState:
    """
    Represents the current state of an agent.

    Attributes:
        status: Current status of the agent
        current_phase: Current work phase
        current_task: Specific task being worked on
        progress: Dictionary tracking progress of subtasks
        waiting_for: List of dependencies waiting on
        blockers: List of current blockers
        errors: List of errors encountered
        timestamp: Last update timestamp
    """

    status: AgentStatus
    current_phase: Optional[WorkPhase] = None
    current_task: Optional[str] = None
    progress: Dict[str, str] = None
    waiting_for: List[Dict[str, str]] = None
    blockers: List[str] = None
    errors: List[str] = None
    timestamp: Optional[str] = None

    def __post_init__(self):
        """Initialize default values for mutable attributes."""
        if self.progress is None:
            self.progress = {}
        if self.waiting_for is None:
            self.waiting_for = []
        if self.blockers is None:
            self.blockers = []
        if self.errors is None:
            self.errors = []
        if self.timestamp is None:
            self.timestamp = datetime.now().isoformat()

    def to_dict(self) -> Dict[str, Any]:
        """
        Convert state to dictionary for JSON serialization.

        Returns:
            Dictionary representation of the state
        """
        data = asdict(self)
        data['status'] = self.status.value
        if self.current_phase:
            data['current_phase'] = self.current_phase.name.lower()
        return data


class Agent:
    """
    Base agent class for SystemG UI team members.

    This class provides the foundation for autonomous agents that can
    execute work, coordinate with other agents, and manage their lifecycle.
    """

    def __init__(
        self,
        role: Union[str, AgentRole],
        instructions_path: Optional[str] = None,
        work_dir: Optional[Path] = None
    ):
        """
        Initialize an Agent instance.

        Args:
            role: The role of this agent (from AgentRole enum or string)
            instructions_path: Path to the markdown instructions file
            work_dir: Working directory for the agent
        """
        if isinstance(role, str):
            try:
                self.role = AgentRole(role.lower())
            except ValueError:
                raise ValueError(f"Invalid role: {role}. Must be one of {[r.value for r in AgentRole]}")
        else:
            self.role = role

        self.instructions_path = Path(instructions_path) if instructions_path else None
        self.work_dir = Path(work_dir) if work_dir else Path.cwd()
        self.snapshot_dir = self.work_dir / "snapshots"
        self.snapshot_dir.mkdir(exist_ok=True)

        self.running = True
        self.state = AgentState(status=AgentStatus.IDLE)
        self.pid = os.getpid()
        self.start_time = datetime.now()

        # Set up signal handlers for graceful shutdown
        signal.signal(signal.SIGTERM, self._handle_shutdown)
        signal.signal(signal.SIGINT, self._handle_shutdown)

        print(f"[{self.role.value}] Agent initialized (PID: {self.pid})")

    def _handle_shutdown(self, signum: int, frame: Any) -> None:
        """
        Handle shutdown signals gracefully.

        Args:
            signum: Signal number
            frame: Current stack frame
        """
        print(f"[{self.role.value}] Received shutdown signal {signum}")
        self.running = False
        self.update_status(AgentStatus.DONE, task="Shutting down")
        sys.exit(0)

    def read_instructions(self) -> str:
        """
        Read instructions from the markdown file.

        Returns:
            The contents of the instructions file

        Raises:
            FileNotFoundError: If instructions file doesn't exist
        """
        if not self.instructions_path or not self.instructions_path.exists():
            raise FileNotFoundError(
                f"Instructions file not found: {self.instructions_path}"
            )
        return self.instructions_path.read_text()

    def update_status(
        self,
        status: AgentStatus,
        phase: Optional[WorkPhase] = None,
        task: Optional[str] = None,
        progress: Optional[Dict[str, str]] = None,
        waiting_for: Optional[List[Dict[str, str]]] = None,
        blockers: Optional[List[str]] = None,
        error: Optional[str] = None
    ) -> None:
        """
        Update agent status and write to signal file.

        Args:
            status: New status for the agent
            phase: Current work phase
            task: Current task description
            progress: Progress dictionary
            waiting_for: List of dependencies
            blockers: List of blockers
            error: Error message if any
        """
        self.state.status = status
        if phase:
            self.state.current_phase = phase
        if task:
            self.state.current_task = task
        if progress:
            self.state.progress.update(progress)
        if waiting_for:
            self.state.waiting_for = waiting_for
        if blockers:
            self.state.blockers = blockers
        if error:
            self.state.errors.append(error)

        self.state.timestamp = datetime.now().isoformat()
        self._write_status_file()

        status_emoji = {
            AgentStatus.IDLE: "⏸",
            AgentStatus.WAITING: "⏳",
            AgentStatus.WORKING: "🔄",
            AgentStatus.DONE: "✅",
            AgentStatus.FAILED: "❌",
            AgentStatus.BLOCKED: "🚧"
        }

        print(f"{status_emoji.get(status, '')} [{self.role.value}] "
              f"Status: {status.value} | Task: {task or 'N/A'}")

    def _write_status_file(self) -> None:
        """Write current status to JSON file for other agents to read."""
        status_file = self.snapshot_dir / f"{self.role.value}.status"
        status_file.write_text(json.dumps(self.state.to_dict(), indent=2))

    def read_team_status(self) -> Dict[str, Dict[str, Any]]:
        """
        Read status files from all team members.

        Returns:
            Dictionary mapping role names to their current status
        """
        team_status = {}
        for status_file in self.snapshot_dir.glob("*.status"):
            role_name = status_file.stem
            try:
                with open(status_file) as f:
                    team_status[role_name] = json.load(f)
            except (json.JSONDecodeError, IOError) as e:
                print(f"[{self.role.value}] Warning: Could not read {role_name} status: {e}")

        return team_status

    def wait_for_condition(
        self,
        condition_fn: callable,
        timeout: Optional[int] = None,
        check_interval: int = 5
    ) -> bool:
        """
        Wait for a condition to become true.

        Args:
            condition_fn: Function that returns True when condition is met
            timeout: Maximum seconds to wait (None for infinite)
            check_interval: Seconds between condition checks

        Returns:
            True if condition was met, False if timed out
        """
        start_time = time.time()

        while self.running:
            if condition_fn():
                return True

            if timeout and (time.time() - start_time) > timeout:
                print(f"[{self.role.value}] Timeout waiting for condition")
                return False

            time.sleep(check_interval)

        return False

    def do_work(self, additional_context: Optional[str] = None) -> subprocess.CompletedProcess:
        """
        Execute Claude with instructions to perform work.

        Args:
            additional_context: Additional context to provide to Claude

        Returns:
            CompletedProcess object with the result
        """
        self.update_status(AgentStatus.WORKING, task="Executing work via Claude")

        # Build the Claude command
        cmd = ["claude", "--dangerously-skip-permissions", "-p"]

        # Create the prompt
        prompt_parts = []

        if self.instructions_path:
            instructions = self.read_instructions()
            prompt_parts.append(f"You are {self.role.value}. Follow these instructions:\n{instructions}")

        if additional_context:
            prompt_parts.append(f"Additional context: {additional_context}")

        prompt_parts.append("Execute your work now. Be concise and efficient.")

        full_prompt = "\n\n".join(prompt_parts)
        cmd.append(full_prompt)

        print(f"[{self.role.value}] Executing work...")
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            cwd=self.work_dir
        )

        if result.returncode != 0:
            self.update_status(
                AgentStatus.FAILED,
                error=f"Claude execution failed: {result.stderr}"
            )

        return result

    def spawn_agent(self, role: AgentRole, instructions_file: str) -> subprocess.Popen:
        """
        Spawn a child agent directly as a subprocess.

        Note: This creates a direct subprocess rather than using sysg spawn
        to maintain process hierarchy without external dependencies.

        Args:
            role: Role of the agent to spawn
            instructions_file: Path to the agent's instructions

        Returns:
            Popen object for the spawned process
        """
        spawn_cmd = [
            sys.executable, __file__,
            "--role", role.value,
            "--instructions", instructions_file
        ]

        print(f"[{self.role.value}] Spawning {role.value} agent...")
        return subprocess.Popen(spawn_cmd, cwd=self.work_dir)

    def verify_work(self) -> bool:
        """
        Verify that work was completed successfully.
        Override in subclasses for specific verification logic.

        Returns:
            True if work is verified, False otherwise
        """
        # Default implementation - check for common success indicators
        if self.role == AgentRole.TEAM_LEAD:
            # Check if npm project exists
            package_json = self.work_dir / "package.json"
            return package_json.exists()

        # For other roles, assume success if no errors in state
        return len(self.state.errors) == 0

    def check_dependencies(self) -> bool:
        """
        Check if dependencies are met for this agent to start work.
        Override in subclasses for specific dependency logic.

        Returns:
            True if dependencies are met, False otherwise
        """
        team_status = self.read_team_status()

        # Role-specific dependency checking
        if self.role in [AgentRole.CORE_INFRA_DEV, AgentRole.UI_DEV, AgentRole.FEATURES_DEV]:
            # Developers need team lead to have completed setup
            if 'team_lead' in team_status:
                tl_status = team_status['team_lead']
                return tl_status.get('status') == 'done' and \
                       tl_status.get('current_phase') == 'project_setup'

        elif self.role == AgentRole.QA_DEV:
            # QA needs all developers to be done
            dev_roles = ['core_infra_dev', 'ui_dev', 'features_dev']
            for role in dev_roles:
                if role not in team_status or team_status[role].get('status') != 'done':
                    return False
            return True

        # No dependencies for owner and team_lead
        return True

    def orchestrate(self) -> None:
        """
        Orchestration logic for managing other agents.
        Override in subclasses that need orchestration capabilities.
        """
        team_status = self.read_team_status()

        if self.role == AgentRole.OWNER:
            # Owner spawns team lead if not running
            if 'team_lead' not in team_status:
                self.spawn_agent(
                    AgentRole.TEAM_LEAD,
                    str(self.work_dir / "instructions" / "TEAM_LEAD.md")
                )

        elif self.role == AgentRole.TEAM_LEAD:
            # Team lead spawns developers after setup
            if self.state.current_phase == WorkPhase.PROJECT_SETUP and \
               self.verify_work():
                dev_roles = [
                    AgentRole.CORE_INFRA_DEV,
                    AgentRole.UI_DEV,
                    AgentRole.FEATURES_DEV
                ]
                for role in dev_roles:
                    if role.value not in team_status:
                        self.spawn_agent(
                            role,
                            str(self.work_dir / "instructions" / f"{role.value.upper()}.md")
                        )

    def run(self) -> None:
        """
        Main execution loop for the agent.

        This method contains the core logic for agent lifecycle:
        1. Check dependencies
        2. Execute work
        3. Verify results
        4. Coordinate with team
        5. Handle failures and retries
        """
        print(f"[{self.role.value}] Starting agent run loop...")
        self.update_status(AgentStatus.IDLE, phase=WorkPhase.INITIALIZING)

        retry_count = 0
        max_retries = 3

        while self.running:
            try:
                # Read team status for coordination
                team_status = self.read_team_status()

                # Check if we're blocked or waiting
                if not self.check_dependencies():
                    if self.state.status != AgentStatus.WAITING:
                        self.update_status(
                            AgentStatus.WAITING,
                            task="Waiting for dependencies"
                        )
                    time.sleep(5)
                    continue

                # Perform orchestration if needed
                self.orchestrate()

                # Do the actual work if not done
                if self.state.status not in [AgentStatus.DONE, AgentStatus.FAILED]:
                    result = self.do_work()

                    if result.returncode == 0 and self.verify_work():
                        self.update_status(
                            AgentStatus.DONE,
                            task="Work completed successfully"
                        )
                        print(f"[{self.role.value}] ✅ Work completed successfully!")
                        break
                    else:
                        retry_count += 1
                        if retry_count >= max_retries:
                            self.update_status(
                                AgentStatus.FAILED,
                                task="Max retries exceeded",
                                error="Failed after maximum retry attempts"
                            )
                            break
                        else:
                            print(f"[{self.role.value}] Retry {retry_count}/{max_retries}...")
                            self.update_status(
                                AgentStatus.WORKING,
                                task=f"Retrying (attempt {retry_count + 1})"
                            )
                            time.sleep(10)  # Wait before retry

                # Check every 10 seconds
                time.sleep(10)

            except KeyboardInterrupt:
                print(f"[{self.role.value}] Interrupted by user")
                self.running = False
            except Exception as e:
                print(f"[{self.role.value}] Error in run loop: {e}")
                self.update_status(
                    AgentStatus.FAILED,
                    error=str(e)
                )
                time.sleep(10)

        print(f"[{self.role.value}] Agent stopped")


def main():
    """Main entry point for the agent script."""
    parser = argparse.ArgumentParser(
        description="SystemG UI Agent Framework",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Run as team lead
  python agent.py --role team_lead --instructions instructions/TEAM_LEAD.md

  # Run as developer
  python agent.py --role ui_dev --instructions instructions/UI_DEV.md

Available roles:
  owner, team_lead, core_infra_dev, ui_dev, features_dev, qa_dev
        """
    )

    parser.add_argument(
        '--role',
        type=str,
        required=True,
        choices=[role.value for role in AgentRole],
        help='Agent role to execute'
    )

    parser.add_argument(
        '--instructions',
        type=str,
        required=True,
        help='Path to the markdown instructions file'
    )

    parser.add_argument(
        '--work-dir',
        type=str,
        default='.',
        help='Working directory for the agent (default: current directory)'
    )

    args = parser.parse_args()

    # Create and run the agent
    agent = Agent(
        role=args.role,
        instructions_path=args.instructions,
        work_dir=Path(args.work_dir)
    )

    try:
        agent.run()
    except Exception as e:
        print(f"Fatal error: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()