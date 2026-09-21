"""Running commands on a switch, locally or in another container.

The single-node suite runs inside the container it tests, the containerlab
suite drives several from outside, and both check the same things in the
kernel. Everything below the `Shell` protocol is written once against it.
"""

import json
import subprocess
from typing import Protocol, Sequence


class CommandError(RuntimeError):
    def __init__(self, where: str, argv: Sequence[str], returncode: int, output: str):
        self.argv = list(argv)
        self.returncode = returncode
        self.output = output
        super().__init__(
            f"{where}: {' '.join(argv)} exited {returncode}\n{output.strip()}"
        )


class Shell(Protocol):
    name: str

    def run(
        self, argv: Sequence[str], *, check: bool = True, timeout: float = 15
    ) -> str:
        """Runs a command and returns its stdout, with stderr folded in."""

    def status(self, argv: Sequence[str], *, timeout: float = 15) -> tuple[int, str]:
        """Runs a command and returns its exit status and output."""

    def succeeds(self, argv: Sequence[str], *, timeout: float = 15) -> bool:
        """Whether the command exited zero."""


class LocalShell:
    """In the switch's own container: the suite runs beside clixon."""

    def __init__(self, name: str = "local"):
        self.name = name

    def _argv(self, argv: Sequence[str]) -> list[str]:
        return list(argv)

    def run(
        self, argv: Sequence[str], *, check: bool = True, timeout: float = 15
    ) -> str:
        returncode, output = self.status(argv, timeout=timeout)
        if check and returncode != 0:
            raise CommandError(self.name, argv, returncode, output)
        return output

    def status(self, argv: Sequence[str], *, timeout: float = 15) -> tuple[int, str]:
        result = subprocess.run(
            self._argv(argv),
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return result.returncode, result.stdout + result.stderr

    def succeeds(self, argv: Sequence[str], *, timeout: float = 15) -> bool:
        return self.status(argv, timeout=timeout)[0] == 0


class DockerShell(LocalShell):
    """Another container, by name: the containerlab nodes."""

    def __init__(self, container: str):
        super().__init__(container)
        self.container = container

    def _argv(self, argv: Sequence[str]) -> list[str]:
        return ["docker", "exec", self.container, *argv]


def run_json(shell: Shell, argv: Sequence[str], *, default=None):
    """Runs a command whose output is JSON. `ip -j` prints "" for nothing."""
    output = shell.run(argv).strip()
    if not output:
        return default
    try:
        return json.loads(output)
    except json.JSONDecodeError as e:
        raise CommandError(shell.name, argv, 0, f"not JSON: {e}\n{output}") from e
