"""
claude-sanitizer.py
Deterministic Claude output sanitizer for the panic-room pipeline.

Removes conversational sludge from LLM-generated text before it
lands in documentation, READMEs, or commit messages.
"""

import re
import sys
from pathlib import Path


# ---------------------------------------------------------------------------
# Markdown-code fallback extractor
# ---------------------------------------------------------------------------

def extract_code_only(raw_llm_response: str) -> str:
    """
    Extract raw code blocks from an unstructured LLM response.

    Structured tool output should be preferred. This function exists only
    as a fallback for responses that arrive as ordinary text.
    """

    pattern = r"```(?:\w+)?\n(.*?)```"
    matches = re.findall(pattern, raw_llm_response, re.DOTALL)

    if matches:
        return "\n\n".join(block.strip() for block in matches)

    return ""


# ---------------------------------------------------------------------------
# Deterministic documentation sanitizer
# ---------------------------------------------------------------------------

CLAUDE_SLUDGE_PATTERNS = [
    r"(?i)^here (is|are) the .*",
    r"(?i)it(?:'s| is) (important|worth) to (note|remember) that.*",
    r"(?i)as an ai language model.*",
    r"(?i)i have (updated|modified|created) the file as requested.*",
    r"(?i)while this (kernel|code|implementation) works, keep in mind.*",
    r"(?i)please note that in a production environment.*",
    r"(?i)^in conclusion,.*",
    r"(?i)^(sure|certainly|absolutely|of course)[,!.].*",
    r"(?i)^i('ll| will) (start|begin|go ahead).*",
    r"(?i)^let me (know|explain|break|walk).*",
    r"(?i)^(this|the) (code|script|function|implementation) (will|does|is).*",
    r"(?i)^below (is|are).*",
    r"(?i)^(the|this) (following|above).*",
    r"(?i)^i hope this.*",
    r"(?i)^feel free to.*",
    r"(?i)^let me know if.*",
    r"(?i)^you can (now|also|then).*",
]


def sanitize_documentation(text: str) -> str:
    """
    Remove known conversational/meta-documentation filler.

    This operates line-by-line and does not rewrite code semantics.
    """

    lines = text.split("\n")
    sanitized_lines = []

    for line in lines:
        stripped = line.strip()

        if any(re.search(pattern, stripped) for pattern in CLAUDE_SLUDGE_PATTERNS):
            continue

        sanitized_lines.append(line)

    return "\n".join(sanitized_lines).strip()


# ---------------------------------------------------------------------------
# File scanner: find and sanitize all markdown/doc files
# ---------------------------------------------------------------------------

MARKDOWN_EXTENSIONS = {".md", ".markdown", ".rst", ".txt"}


def sanitize_repository(root: Path) -> int:
    """
    Walk the repository and sanitize all documentation files in place.
    Returns the number of files modified.
    """
    modified = 0

    for path in root.rglob("*"):
        if path.is_file() and path.suffix in MARKDOWN_EXTENSIONS:
            # skip .git and target directories
            if any(part in {".git", "target", "node_modules", ".panic-room"}
                   for part in path.parts):
                continue

            try:
                original = path.read_text(encoding="utf-8")
            except Exception:
                continue

            cleaned = sanitize_documentation(original)

            if cleaned != original:
                path.write_text(cleaned, encoding="utf-8")
                print(f"  sanitized: {path.relative_to(root)}")
                modified += 1

    return modified


# ---------------------------------------------------------------------------
# README generator stub
# ---------------------------------------------------------------------------

def generate_readme(root: Path) -> None:
    """
    Auto-generate or update README.md from the repository's semantic
    snapshot. Delegates to panic-room for the heavy lifting.
    """
    readme_path = root / "README.md"

    # Collect contract statements from markdown files for README
    contracts = []
    for md in sorted(root.glob("*.md")):
        if md.name == "README.md":
            continue
        try:
            text = md.read_text(encoding="utf-8")
            for line in text.splitlines():
                stripped = line.strip()
                if any(kw in stripped.lower() for kw in ("must", "shall", "invariant")):
                    contracts.append(f"- {stripped}")
        except Exception:
            continue

    if not contracts:
        return

    header = f"# {root.name}\n\n## Contracts\n\n"
    body = "\n".join(contracts[:50])  # cap at 50

    # Only write if changed
    new_content = header + body + "\n"
    if readme_path.exists():
        existing = readme_path.read_text(encoding="utf-8")
        if existing == new_content:
            return

    readme_path.write_text(new_content, encoding="utf-8")
    print(f"  generated: {readme_path.relative_to(root)}")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main() -> int:
    if len(sys.argv) < 2:
        print("usage: claude-sanitizer.py <repo-root> [--generate-readme]")
        return 2

    root = Path(sys.argv[1]).resolve()

    if not root.is_dir():
        print(f"error: {root} is not a directory", file=sys.stderr)
        return 1

    print(f"panic-room sanitizer: {root}")

    modified = sanitize_repository(root)
    print(f"  files sanitized: {modified}")

    if "--generate-readme" in sys.argv:
        generate_readme(root)

    return 0


if __name__ == "__main__":
    sys.exit(main())