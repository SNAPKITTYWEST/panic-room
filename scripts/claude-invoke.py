"""
claude-invoke.py
Structured Claude invocation for the panic-room pipeline.

Calls Claude via the Anthropic API with a forced tool-call to get
clean, structured code output. Sanitizes the result before writing
to disk, then runs panic-room to snapshot semantic continuity.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

try:
    import anthropic
except ImportError:
    print("error: anthropic package not installed. Run: pip install anthropic", file=sys.stderr)
    sys.exit(1)

from claude_sanitizer import extract_code_only, sanitize_documentation


# ---------------------------------------------------------------------------
# Tool definition: raw code/documentation payload
# ---------------------------------------------------------------------------

COMMIT_CODE_PAYLOAD_TOOL = {
    "name": "commit_code_payload",
    "description": (
        "Writes raw code to disk. "
        "Do NOT include explanations or comments outside the code payload."
    ),
    "input_schema": {
        "type": "object",
        "properties": {
            "file_path": {
                "type": "string",
                "description": "Scrubbed destination file path."
            },
            "raw_code": {
                "type": "string",
                "description": (
                    "Bit-exact code or documentation. "
                    "No backticks, no chat, no wrapper text."
                )
            }
        },
        "required": ["file_path", "raw_code"]
    }
}


# ---------------------------------------------------------------------------
# Claude invocation
# ---------------------------------------------------------------------------

def invoke_claude(
    prompt: str,
    model: str = "claude-3-5-haiku-20241022",
    system: str = (
        "You are a silent code compiler. "
        "Output zero conversational text. "
        "Execute the tool call directly."
    ),
    max_tokens: int = 8192,
) -> list[dict]:
    """
    Call Claude with forced tool-use and return the list of
    commit_code_payload inputs from the response.
    """
    client = anthropic.Anthropic()

    response = client.messages.create(
        model=model,
        max_tokens=max_tokens,
        system=system,
        messages=[{"role": "user", "content": prompt}],
        tools=[COMMIT_CODE_PAYLOAD_TOOL],
        tool_choice={"type": "tool", "name": "commit_code_payload"},
    )

    payloads = []
    for block in response.content:
        if block.type == "tool_use" and block.name == "commit_code_payload":
            payloads.append(block.input)

    return payloads


# ---------------------------------------------------------------------------
# Write payloads to disk
# ---------------------------------------------------------------------------

def write_payloads(payloads: list[dict], repo_root: Path) -> list[Path]:
    """
    Write each commit_code_payload to disk under repo_root.
    Sanitizes the raw_code before writing.
    Returns list of written file paths.
    """
    written = []

    for payload in payloads:
        relative = payload["file_path"]
        raw_code = payload["raw_code"]

        sanitized = sanitize_documentation(raw_code)
        if not sanitized:
            fallback = extract_code_only(raw_code)
            if fallback:
                sanitized = fallback
            else:
                sanitized = raw_code

        target = repo_root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(sanitized, encoding="utf-8")
        written.append(target)
        print(f"  wrote: {relative}")

    return written


# ---------------------------------------------------------------------------
# Panic-room integration
# ---------------------------------------------------------------------------

def panic_room_snapshot(repo_root: Path) -> bool:
    """Run panic-room snapshot and return True on success."""
    try:
        result = subprocess.run(
            ["panic-room", "snapshot"],
            cwd=str(repo_root),
            capture_output=True,
            text=True,
            timeout=60,
        )
        if result.returncode == 0:
            print(result.stdout.strip())
            return True
        else:
            print(f"  panic-room snapshot failed: {result.stderr.strip()}", file=sys.stderr)
            return False
    except FileNotFoundError:
        print("  warning: panic-room binary not found; skipping snapshot", file=sys.stderr)
        return False


def panic_room_check(repo_root: Path, old_commit: str, new_commit: str) -> bool:
    """Run panic-room check and return True if PASS."""
    try:
        result = subprocess.run(
            ["panic-room", "check", old_commit, new_commit],
            cwd=str(repo_root),
            capture_output=True,
            text=True,
            timeout=60,
        )
        print(result.stdout.strip())
        if result.returncode == 0:
            return True
        else:
            print(f"  PANIC: {result.stderr.strip()}", file=sys.stderr)
            return False
    except FileNotFoundError:
        print("  warning: panic-room binary not found; skipping check", file=sys.stderr)
        return False


# ---------------------------------------------------------------------------
# Pipeline
# ---------------------------------------------------------------------------

def run_pipeline(prompt: str, repo_root: Path, model: str = "claude-3-5-haiku-20241022") -> int:
    """
    Full panic-room pipeline:
      1. Invoke Claude -> structured code payloads
      2. Sanitize and write to disk
      3. Snapshot semantic state
      4. If prior snapshot exists, check continuity
    """
    repo_root = repo_root.resolve()

    print(f"panic-room pipeline: {repo_root}")
    print(f"  model: {model}")
    print()

    # Step 1: Invoke Claude
    print("invoking Claude...")
    payloads = invoke_claude(prompt, model=model)

    if not payloads:
        print("error: no structured payloads returned", file=sys.stderr)
        return 1

    print(f"  payloads received: {len(payloads)}")
    print()

    # Step 2: Sanitize and write
    print("writing sanitized payloads...")
    written = write_payloads(payloads, repo_root)
    print(f"  files written: {len(written)}")
    print()

    # Step 3: Sanitize any existing documentation in the repo
    print("sanitizing repository documentation...")
    from claude_sanitizer import sanitize_repository
    modified = sanitize_repository(repo_root)
    print(f"  files sanitized: {modified}")
    print()

    # Step 4: Panic-room snapshot
    print("creating semantic snapshot...")
    snapshot_ok = panic_room_snapshot(repo_root)

    if not snapshot_ok:
        print("  snapshot skipped; pipeline complete (no continuity check)")
        return 0

    print()

    # Step 5: Optional continuity check against previous commit
    try:
        git_result = subprocess.run(
            ["git", "rev-parse", "HEAD~1"],
            cwd=str(repo_root),
            capture_output=True,
            text=True,
            timeout=10,
        )
        if git_result.returncode == 0:
            old_commit = git_result.stdout.strip()
            new_result = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=str(repo_root),
                capture_output=True,
                text=True,
                timeout=10,
            )
            new_commit = new_result.stdout.strip()

            print(f"checking continuity {old_commit[:8]}..{new_commit[:8]}")
            passed = panic_room_check(repo_root, old_commit, new_commit)

            if passed:
                print("status: PASS")
                return 0
            else:
                print("status: PANIC")
                return 1
    except Exception:
        pass

    print("pipeline complete (no prior commit for continuity check)")
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> int:
    if len(sys.argv) < 3:
        print("usage: claude-invoke.py <repo-root> <prompt> [--model MODEL]")
        print()
        print("  repo-root    Path to the target repository")
        print("  prompt       Instruction to send to Claude")
        print("  --model      Claude model (default: claude-3-5-haiku-20241022)")
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
