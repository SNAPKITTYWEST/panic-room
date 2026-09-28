"""
pipeline.py
Top-level orchestrator for the panic-room pipeline.

Usage:
    python pipeline.py <repo-root> <prompt> [--model MODEL]

Pipeline stages:
    1. Claude invocation (forced tool-call output)
    2. Sludge sanitization on all output
    3. Write clean code to disk
    4. Repository-wide doc sanitization
    5. Panic-room semantic snapshot
    6. Continuity check (PASS / PANIC)
"""

import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "scripts"))

from claude_invoke import run_pipeline


def main() -> int:
    if len(sys.argv) < 3:
        print("panic-room pipeline")
        print()
        print("usage: pipeline.py <repo-root> <prompt> [--model MODEL]")
        print()
        print("Stages:")
        print("  1. Claude API  -> structured code payloads")
        print("  2. Sanitize    -> strip conversational sludge")
        print("  3. Write       -> clean code to disk")
        print("  4. Repo scan   -> sanitize existing docs")
        print("  5. Snapshot    -> panic-room semantic snapshot")
        print("  6. Check       -> continuity PASS/PANIC")
        return 2

    repo_root = Path(sys.argv[1])
    prompt = sys.argv[2]
    model = "claude-3-5-haiku-20241022"

    if "--model" in sys.argv:
        idx = sys.argv.index("--model")
        if idx + 1 < len(sys.argv):
            model = sys.argv[idx + 1]

    if not repo_root.is_dir():
        print(f"error: {repo_root} is not a directory", file=sys.stderr)
        return 1

    return run_pipeline(prompt, repo_root, model)


if __name__ == "__main__":
    sys.exit(main())
