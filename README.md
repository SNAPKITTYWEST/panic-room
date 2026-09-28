# panic-room

Semantic continuity engine + LLM output sanitizer.

Frontier models inject conversational filler into every response —
"Here is the code...", "It's important to note...", "As an AI language
model..." — even when you never asked for commentary. **panic-room**
strips it all out at the structural level, then snapshots your repo's
semantic state so you can detect when a model's output has drifted
your codebase off-contract.

Built for people who run local LLMs, Claude, or any frontier model
and want **clean output, not a conversation.**

---

## What it does

### 1. Sludge Sanitizer (`scripts/claude-sanitizer.py`)

Deterministic regex-based removal of 16 known conversational filler
patterns that frontier models emit regardless of your prompt:

```
Here is the implementation...     ← gone
It's important to note that...    ← gone
As an AI language model...        ← gone
I have updated the file as...     ← gone
While this code works, keep...    ← gone
Please note that in production... ← gone
Sure, I'll start by...            ← gone
Let me explain...                 ← gone
Below is the...                   ← gone
I hope this helps...              ← gone
Feel free to...                   ← gone
```

Operates line-by-line. Never rewrites code semantics. Can walk an
entire repository and sanitize all `.md`/`.rst`/`.txt` files in place.

```sh
python scripts/claude-sanitizer.py /path/to/repo
python scripts/claude-sanitizer.py /path/to/repo --generate-readme
```

### 2. Structured Claude Invoker (`scripts/claude-invoke.py`)

Forces Claude to output **structured tool-call payloads** instead of
conversational text. Uses Anthropic's `tool_choice` to mandate a
`commit_code_payload` tool call — Claude can't chat, it can only
emit `file_path` + `raw_code`.

The sanitizer runs on every payload before it touches disk.

### 3. Pipeline (`pipeline.py`)

Wires everything together:

```
Claude API (forced tool-call)
  → sludge sanitization
  → clean code to disk
  → repo-wide doc sanitization
  → semantic snapshot (Rust engine)
  → continuity check: PASS / PANIC
```

```sh
python pipeline.py /path/to/repo "Write the C11 GGUF header parser"
```

### 4. Rust Semantic Engine (`src/main.rs`)

Scans Rust source via `syn`, builds a semantic graph, and snapshots
per-commit. Compares snapshots using a deterministic SHA256-based
bi-encoder (256-dim vectors, no external model dependency). Flags
`PANIC` when embedding similarity drops below threshold.

```sh
panic-room snapshot              # snapshot at HEAD
panic-room check HEAD~1 HEAD     # PASS / PANIC
panic-room diff HEAD~1 HEAD      # git diff summary
panic-room status                # HEAD + snapshot count
panic-room install               # install pre-push hook
```

---

## Why this exists

Frontier model providers are increasingly restricting local model
access, imposing output filters, and injecting conversational filler
that makes LLM output unsuitable for direct use in codebases. If
you're running Claude, GPT, or any frontier model through an API and
piping output into your repo, you need:

1. **Structural enforcement** — force tool-use, not chat
2. **Deterministic cleanup** — strip the filler that leaks through
3. **Semantic verification** — know when model output breaks your
   codebase's contracts

panic-room does all three. No central coordinator, no cloud dependency,
no external model calls (the bi-encoder is deterministic SHA256).

---

## Install

### Rust engine

```sh
cargo build --release
```

### Python tools

```sh
pip install anthropic    # required for claude-invoke.py / pipeline.py
```

### Environment

| Variable | Purpose |
|---|---|
| `ANTHROPIC_API_KEY` | Required for Claude API calls |
| `PANIC_ROOM_DSPY_URL` | Optional DSPy semantic-analysis endpoint |

---

## Contracts

- The repository must produce deterministic snapshots for the same source tree
- Snapshots shall be stored under `.panic-room/snapshots/` keyed by commit SHA
- The continuity engine must flag PANIC when embedding similarity drops below 0.60
- Contract similarity below 0.70 shall trigger PANIC
- The sanitizer must not rewrite code semantics, only strip conversational filler
- Claude invocations must use forced tool-choice to prevent conversational output

---

## License

MIT — use it, fork it, ship it. Fight the filters.
