# Shared provider programs

These Python programs are embedded with `include_str!` by
[`tests/common/mod.rs`](../../common/mod.rs). The harness installs their contents
in each test's temporary directory and supplies the test-specific arguments,
environment and file paths. Keep setup, cleanup and assertions in the Rust
harness and test callers; these assets implement the simulated provider protocol.

| Asset | Harness constant | Purpose |
| --- | --- | --- |
| `codex.py` | `MOCK_PY` | Codex app-server over stdio |
| `codex-websocket.py` | `MOCK_WS_PY` | Codex app-server over WebSocket |
| `tmux.py` | `MOCK_TMUX_PY` | File-backed pane control and capture |
| `devin.py` | `MOCK_DEVIN_PY` | Devin terminal prompt and input recording |
| `stub.py` | `MOCK_STUB_PY` | Minimal terminal profile |
| `claude-tui.py` | `MOCK_CLAUDE_TUI_PY` | Claude terminal session and prompt |
| `cursor-tui.py` | `MOCK_CURSOR_TUI_PY` | Cursor terminal chat and prompt |
| `claude-managed.py` | `MOCK_CLAUDE_PY` | Claude managed stream-json endpoint |
| `enroll.py` | `MOCK_ENROLL_PY` | Managed worker enrollment and reporting |

The first extraction preserved every program byte, including initial newlines.
The Rust constant names and installer paths remain stable, so callers still use
the same harness APIs. Future behavior changes should be checked through the
actual consumer: `managed_claude_codex`, `agents_pty`, `pty_lifecycle`, or the
`build_slot` and `reports_memory` tests that call `ManagedWorker`. Other fixtures such
as `tests/e2e/fake-pi.py` retain their existing location and consumers.

This separation improves navigation and language-aware editing. It does not
reduce the number of integration crates compiling `mod common`, and it makes no
claim about build-time or runtime savings. A support-library extraction needs
separate profiling and a dependency plan, including passing the cadence binary
path from the integration target instead of moving its
`env!("CARGO_BIN_EXE_cadence")` into a library.
