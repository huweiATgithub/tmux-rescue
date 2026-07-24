# tmux-rescue v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the complete manual snapshot and plan-first restore CLI defined by the approved v1 design.

**Architecture:** One Rust package exposes a reusable `tmux_rescue` library and a thin `tmux-rescue` binary. Opaque validated snapshot types sit between untrusted JSON and all planning; external tmux, process, filesystem, clock, and shell operations are narrow capabilities with real Linux/tmux adapters and deterministic test doubles.

**Tech Stack:** Rust 1.94, edition 2024, tmux 3.4, Linux `/proc`, `serde`/`serde_json`, `clap`, `thiserror`, `base64`, `time`, `uuid`, and `libc`.

## Global Constraints

- The live authorities are `docs/src/DESIGN.md`, `docs/src/ARCHITECTURE.md`, and `docs/src/TOOL-RECOVERIES.md`.
- Work directly on the user-approved `main` checkout; do not create commits or push unless the user asks.
- Follow red-green-refactor for every production behavior: add one failing test, observe the expected failure, implement the minimum behavior, and rerun the focused and full suites.
- The library does not parse CLI arguments, print, or terminate the process.
- Every snapshot, including `latest` and explicit paths, is untrusted input refined into opaque validated types.
- v1 has no schema-version field, automatic capture, restore into an existing server, exact tmux layout, environment restore, reminders, or automatic deletion.
- Topology equality is exactly `session_name -> window_index -> pane_index`; `MAX_TOPOLOGY_VALIDATION_ATTEMPTS` is `3`.
- All tmux integration tests use unique temporary socket paths and temporary state roots; they never address the user's default server.
- Automatic execution is limited to the four variants in `TOOL-RECOVERIES.md`; every other readable foreground command is manual.
- Concrete defensive limits are implementation policy constants, not scattered literals: 16 MiB per snapshot, 1,024 sessions, 1,024 windows per session, 1,024 panes per window, 1 MiB per lossless OS value, 4 KiB per diagnostic, and a 2-second automatic-recovery settle interval.

## File Map

- `Cargo.toml`: package metadata and minimal dependencies.
- `src/lib.rs`: public library surface and orchestration exports.
- `src/model.rs`: lossless OS values, raw serialized shapes, opaque validated snapshot tree, and recovery variants.
- `src/recovery.rs`: whitelist classification, Codex/Claude metadata parsing, serve-command refinement, and success expectations.
- `src/process.rs`: Linux foreground process and `/proc` inspection.
- `src/capture.rs`: topology fingerprinting, bounded capture retries, candidate selection, and capture events.
- `src/storage.rs`: immutable publication, advisory pointer locking, `latest` selection, and bounded loading.
- `src/restore.rs`: target-shell rendering, refined restore plans, execution outcomes, and best-effort recovery.
- `src/tmux.rs`: real tmux source/target adapter and ownership-scoped operations.
- `src/cli.rs`: binary-local command orchestration and human-readable reports.
- `src/main.rs`: Clap parsing and exit-code mapping only.
- `tests/*.rs`: public behavior, storage, planner, CLI, and isolated tmux integration tests.

---

### Task 1: Package And Validated Snapshot Model

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/model.rs`
- Create: `tests/model.rs`

**Interfaces:**
- Produces: `LosslessOsString`, `CapturedCommand`, `AutomaticRecovery`, `RawSnapshot`, `ValidatedSnapshot`, `SnapshotValidationError`, and `ValidatedSnapshot::from_json(&[u8])`.
- Invariant: `ValidatedSnapshot` and nested constructors remain private; public access is read-only.

- [x] **Step 1: Add the package manifest and a failing public model test**

Use this initial manifest so dependency choices are explicit:

```toml
[package]
name = "tmux-rescue"
version = "0.1.0"
edition = "2024"
rust-version = "1.94"

[dependencies]
base64 = "0.22"
clap = { version = "4.5", features = ["derive"] }
libc = "0.2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
time = { version = "0.3", features = ["formatting", "parsing", "serde"] }
uuid = { version = "1", features = ["v4"] }

[dev-dependencies]
tempfile = "3"
```

```rust
use tmux_rescue::{SnapshotValidationError, ValidatedSnapshot};

#[test]
fn rejects_an_empty_session_tree() {
    let raw = br#"{
      "captured_at":"2026-07-23T00:00:00Z",
      "source":{"encoding":"utf8","value":"/tmp/source.sock"},
      "consistency":{"kind":"stable"},
      "sessions":[]
    }"#;

    assert_eq!(
        ValidatedSnapshot::from_json(raw).unwrap_err(),
        SnapshotValidationError::EmptySessions
    );
}
```

Run: `cargo test --test model rejects_an_empty_session_tree`  
Expected: compile failure because the library API does not exist.

- [x] **Step 2: Implement lossless encoded OS values and raw JSON shapes**

Use the tagged representation `{"encoding":"utf8","value":"..."}` when bytes are UTF-8 and `{"encoding":"base64","value":"..."}` otherwise. Reject decoded NUL bytes, over-limit fields, invalid base64, and relative source/CWD paths.

Run: `cargo test --test model rejects_an_empty_session_tree`  
Expected: the test passes.

- [x] **Step 3: Add failing invariant and round-trip tests**

Cover duplicate session names, duplicate window indexes, duplicate pane indexes, empty windows/panes, relative paths, an unstable attempt count other than `MAX_TOPOLOGY_VALIDATION_ATTEMPTS`, empty argv collections, an empty `argv[0]`, preserved empty later arguments, and non-UTF-8 byte round trips.

Run: `cargo test --test model`  
Expected: new tests fail on the first unimplemented invariant.

- [x] **Step 4: Complete opaque refinement and read-only accessors**

Define the exact recovery sum:

```rust
pub enum AutomaticRecovery {
    Codex { session_id: CodexSessionId },
    ClaudeCode { session_id: ClaudeSessionId },
    MdBookServe { command: RecognizedMdBookServeCommand },
    BookshelfServe { command: RecognizedBookshelfServeCommand },
}
```

Validate UUID-shaped session IDs, serve-command coherence, bounded collections/strings, required window names, absolute paths, and scoped uniqueness.

Run: `cargo test`  
Expected: all model tests pass with no warnings.

### Task 2: Whitelist Recovery Classification

**Files:**
- Create: `src/recovery.rs`
- Create: `tests/recovery.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: model types from Task 1.
- Produces: `PaneTiedForegroundEvidence`, `ResolverOutcome`, `classify_pane`, `derive_automatic_command`, and `AutomaticRecoveryExpectation`.

- [x] **Step 1: Write failing serve and fallback classification tests**

```rust
#[test]
fn recognizes_only_exact_mdbook_serve_argv() {
    let evidence = evidence("/usr/bin/mdbook", &["mdbook", "serve", "-p", "3000"]);
    assert!(matches!(
        classify_pane(evidence, None),
        PaneRecovery::Automatic(AutomaticRecovery::MdBookServe { .. })
    ));
}

#[test]
fn downgrades_other_readable_commands_to_manual() {
    let evidence = evidence("/usr/bin/mdbook", &["mdbook", "build"]);
    assert!(matches!(classify_pane(evidence, None), PaneRecovery::Manual(_)));
}
```

Run: `cargo test --test recovery recognizes_only_exact_mdbook_serve_argv`  
Expected: compile failure for missing recovery API.

- [x] **Step 2: Implement Idle, Manual, Unavailable, mdBook, and Bookshelf classification**

Require both inspected executable basename and `argv[0]` basename to match, with `argv[1] == "serve"`. Preserve the complete command for every manual downgrade.

Run: `cargo test --test recovery`  
Expected: serve and fallback tests pass.

- [x] **Step 3: Add failing Codex and Claude fixture tests**

Codex fixtures require a process-opened JSONL file whose first record has exact `session_meta`, `codex-tui`, `user`, matching CWD, null/absent parent, and UUID `payload.id`. Claude fixtures require either exact live `--session-id`/`--resume` argv or the complete worker evidence tuple from the design.

Run: `cargo test --test recovery codex`  
Expected: failures because session resolvers are absent.

- [x] **Step 4: Implement exact session resolvers, command derivation, and tail conflict**

Derive `["codex", "resume", id]` and `["claude", "--resume", id]` rather than serializing duplicate commands. Treat an explicit different tool-attributed tail UUID as `ConflictingEvidence`; missing/inconclusive tail remains neutral.

Run: `cargo test`  
Expected: all recovery tests pass.

### Task 3: Capture Retry Engine

**Files:**
- Create: `src/capture.rs`
- Create: `tests/capture.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `PaneRecovery` and validated snapshot constructors.
- Produces: `CaptureSource` capability, `capture_snapshot`, `CaptureEvent`, `CaptureResult`, and `MAX_TOPOLOGY_VALIDATION_ATTEMPTS`.

- [x] **Step 1: Write failing stable and retry tests using a complete scripted source**

The scripted source returns full topology records and pane evidence, not partial mocks. Assert one attempt for matching fingerprints and a full recapture after a mismatch.

Run: `cargo test --test capture stable_capture`  
Expected: compile failure for missing capture engine.

- [x] **Step 2: Implement structural fingerprinting and stable capture**

Fingerprint only session name, window index, and pane index. Build the candidate from the before-read records and refined pane recoveries; do not compare names, CWDs, or processes.

Run: `cargo test --test capture`  
Expected: stable and retry tests pass.

- [x] **Step 3: Add failing exhaustion/failure tests**

Cover mismatches through `MAX_TOPOLOGY_VALIDATION_ATTEMPTS` publishing the most recent complete candidate as `Unstable { attempts: ExhaustedAttemptCount }`, failed after-reads retaining a complete candidate, failed before-reads producing no candidate, and exhaustion without a complete candidate returning a fatal capture error.

Run: `cargo test --test capture`  
Expected: the first exhaustion case fails.

- [x] **Step 4: Complete typed attempt outcomes and capture events**

Emit every mismatch/read/process downgrade through returned `CaptureEvent` values. A pane inspection failure becomes `Unavailable` and does not invalidate an otherwise complete candidate.

Run: `cargo test`  
Expected: all capture tests pass.

### Task 4: Linux Process And Source tmux Adapters

**Files:**
- Create: `src/process.rs`
- Create: `src/tmux.rs`
- Create: `tests/process_linux.rs`
- Create: `tests/tmux_source.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `CaptureSource` and `PaneTiedForegroundEvidence`.
- Produces: `LinuxProcessInspector`, `TmuxAdapter::selected_source()`, and the real `CaptureSource` implementation.

- [x] **Step 1: Write failing `/proc` parsing tests**

Use captured `stat`/`cmdline` byte fixtures to verify `comm` values containing spaces and `)`, field-22 start time parsing, fields 5-8 job-control parsing, NUL-delimited argv with empty elements, executable/CWD byte preservation, and foreground process-group selection.

Run: `cargo test --test process_linux`  
Expected: compile failure for missing parser.

- [x] **Step 2: Implement Linux inspection**

Read the pane process's `/proc/<pid>/stat` `tpgid` (a non-controlling inspector cannot rely on `tcgetpgrp`), enumerate same-tty/session/group members, require one tree rooted at the live process-group leader, and read `/proc/<pid>/{stat,cmdline,exe,cwd,fd}` with a stability reread. Persist only the leader command; group members remain transient resolver evidence. Return typed unreadable/ambiguous/raced failures.

Run: `cargo test --test process_linux`  
Expected: fixture tests pass.

- [x] **Step 3: Write a failing isolated source-tmux test**

Start a unique `tmux -S <temp-socket>` server with one named session/window and two panes in different CWDs. Assert capture records session path, window index/name, pane indexes/CWDs, pane PID/TTY, and the exact structural fingerprint. Always kill only that socket in test cleanup.

Run: `cargo test --test tmux_source -- --nocapture`  
Expected: compile failure or assertion failure because the adapter is absent.

- [x] **Step 4: Implement source selection and quoted record parsing**

Resolve the invoking server with `tmux -u -N display-message` and `#{socket_path}`, then clear inherited `TMUX` and pin all later commands to `-u -N -S <path>`. Parse one `list-panes -a` response using tmux 3.4's `#{n:field}:#{field}` byte-length prefixes for `session_name`, `session_path`, `window_index`, `window_name`, `pane_index`, `pane_current_path`, and `pane_pid`; reject malformed, duplicate, or conflicting records before grouping them.

Run: `cargo test`  
Expected: unit and isolated source-tmux tests pass.

### Task 5: Durable Snapshot Storage

**Files:**
- Create: `src/storage.rs`
- Create: `tests/storage.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `ValidatedSnapshot` serialization.
- Produces: `StateStore`, `SnapshotPublication`, `LatestDisposition`, `LoadedSnapshot`, and `MAX_SNAPSHOT_BYTES`.

- [x] **Step 1: Write failing immutable-publication tests**

Assert owner-only roots/files, `<timestamp>-<uuid>.json` names, no replacement on collision, a relative `latest` symlink, and round-trip loading from the same opened regular file.

Run: `cargo test --test storage publishes_immutable_snapshot`  
Expected: compile failure for missing storage API.

- [x] **Step 2: Implement committed immutable publication**

Write and sync a mode-0600 temporary file, create the final name without replacement, sync `snapshots/`, and return `PublicationIndeterminate` only when the final entry may exist but directory durability is unknown.

Run: `cargo test --test storage`  
Expected: publication tests pass.

- [x] **Step 3: Add failing pointer ordering and hostile-link tests**

Cover advisory-lock serialization, newer/equal/clock-regressed capture keys, invalid/dangling/absolute/escaping links, replacement of an invalid pointer, and swapping `latest` after selection without changing the already opened file.

Run: `cargo test --test storage`  
Expected: the first pointer-order test fails.

- [x] **Step 4: Implement atomic pointer updates and bounded loading**

Hold the short lock only around pointer comparison/swap; create a unique temporary symlink and atomically rename it. Enforce file-size and collection bounds before/while parsing and open the selected regular file beneath `snapshots/` without following a final symlink.

Run: `cargo test`  
Expected: all storage and model tests pass.

### Task 6: Restore Planning And Shell Rendering

**Files:**
- Create: `src/restore.rs`
- Create: `tests/restore_plan.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `ValidatedSnapshot` and derived automatic commands.
- Produces: `RestoreEnvironment`, `RestorePlan`, `PlannedPaneAction`, `TargetShell`, `RenderedShellInput`, and `plan_restore`.

- [x] **Step 1: Write failing plan-only safety tests**

Assert that an existing target is rejected even without `--run`, missing session and pane directories use the documented fallbacks, and an automatic action with a fallback CWD or missing executable becomes `PasteAutomaticFallback`.

Run: `cargo test --test restore_plan rejects_existing_target`  
Expected: compile failure for missing planner.

- [x] **Step 2: Implement target-shell selection and safe argv rendering**

Select the first suitable absolute executable from `SHELL`, `getpwuid(3)`, then `/bin/sh`. Render each argv element with POSIX single-quote escaping into bytes; reject NUL, CR, LF, and other terminal-control bytes rather than producing unsafe input. Bind rendered input to `TargetShell`.

Run: `cargo test --test restore_plan`  
Expected: shell and basic planner tests pass.

- [x] **Step 3: Add failing refined-action and degradation tests**

Cover `LeaveIdle`, `LaunchAutomatic`, `PasteManualHint`, `PasteAutomaticFallback`, and `NoInput`; prove only `LaunchAutomatic` contains an existing recorded directory and launchable executable.

Run: `cargo test --test restore_plan`  
Expected: first missing action mapping fails.

- [x] **Step 4: Complete opaque restore-plan construction and display**

Preserve session/window/pane order, source indexes, names, independent CWD resolution, exact automatic expectations, named degradations, and a deterministic human-readable plan used by both plan-only and `--run`.

Run: `cargo test`  
Expected: all planner tests pass.

### Task 7: Owned Target And Best-Effort Restore Execution

**Files:**
- Modify: `src/restore.rs`
- Modify: `src/tmux.rs`
- Create: `tests/restore_execute.rs`
- Create: `tests/tmux_target.rs`

**Interfaces:**
- Consumes: opaque `RestorePlan`.
- Produces: `RestoreExecutor`, `OwnedRestoreTarget`, `PaneRestoreOutcome`, `RestoreRunResult`, and the real target adapter.

- [x] **Step 1: Write failing ownership and rollback tests**

Use a complete in-memory target capability to prove pre-existing/indeterminate targets never yield ownership, every topology mutation requires the ownership token, topology failure consumes it for rollback, and cleanup failure reports `Observed(Retained|Unknown)`.

Run: `cargo test --test restore_execute ownership`  
Expected: compile failure for missing executor.

- [x] **Step 2: Implement the two-phase executor**

Recheck immediately before claim. Claim a vacant target (a missing path or refused crash socket) with `start-server` plus an owner-only temporary config containing a random `@tmux_rescue_owner` token, then read the token and server identity back; command success alone is not ownership proof. Guard every later mutation by token, PID, and start time. Create all sessions with session CWDs, windows at recorded indexes/names, panes in source order with pane CWDs, and explicit target shell. Roll back only before recovery and only through `OwnedRestoreTarget`.

Run: `cargo test --test restore_execute`  
Expected: topology and rollback tests pass.

- [x] **Step 3: Add failing guarded-input and best-effort tests**

Cover shell-to-program change during input guard, literal hint without Enter, automatic submission with a separate Enter, exact post-launch identity success, failed launch returning to shell and preparing a hint, unexpected foreground process receiving no input, missing pane, unavailable recovery, and continuation to later panes.

Run: `cargo test --test restore_execute guarded_input`  
Expected: first guarded-input behavior fails.

- [x] **Step 4: Implement guarded recovery and aggregate outcomes**

Make observation and conditional send one target-adapter operation. Emit `RestoredIdleShell`, `RecoveredAutomatically`, `PreparedManualHint`, `PreparedAutomaticFallbackHint`, `AutomaticLaunchFailedHintPrepared`, or `NeedsAttention` for every planned pane and derive complete/partial/fatal results.

Run: `cargo test --test restore_execute`  
Expected: fake-capability tests pass.

- [x] **Step 5: Add and pass isolated target-tmux tests**

Claim a unique socket with a per-restore token, prove an already-running socket is unchanged, create multi-session topology, verify window names/CWDs and interactive shells, test literal send/Enter separation, and remove only the owned server during cleanup.

Run: `cargo test --test tmux_target -- --nocapture`  
Expected: all isolated target tests pass.

### Task 8: Thin CLI And End-To-End Workflow

**Files:**
- Create: `src/cli.rs`
- Create: `src/main.rs`
- Create: `tests/cli.rs`
- Create: `tests/e2e.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: capture, storage, restore planning/execution, and real adapters.
- Produces: `run_snapshot`, `run_restore`, CLI output reports, and exit-code mapping `0/1/2`.

- [x] **Step 1: Write failing parser and plan-only CLI tests**

Assert exact forms `tmux-rescue snapshot` and `tmux-rescue restore [SNAPSHOT] [--target <server>] [--run]`, default restore without `--run`, rejection of existing targets during plan, and no JSON-output flag.

Run: `cargo test --test cli`  
Expected: compile failure because the CLI target is absent.

- [x] **Step 2: Implement the thin binary and stream/exit mapping**

Keep orchestration in `src/cli.rs`. Send snapshot path/consistency/latest disposition, restore plan, and final inventory to stdout; send progress, warnings, fatal diagnostics, and pane failures to stderr. Map snapshot success/failure to `0/1` and restore complete/fatal/partial to `0/1/2`.

Run: `cargo test --test cli`  
Expected: CLI contract tests pass.

- [x] **Step 3: Write a failing isolated end-to-end test**

Create a uniquely addressed source server containing idle, manual, and whitelisted serve panes; invoke the real binary with a temporary XDG state root; verify an immutable snapshot and `latest`; verify restore defaults to a printed plan; run restore to a second target with no live server; inspect reconstructed topology and hints; clean both sockets.

Run: `cargo test --test e2e -- --nocapture`  
Expected: the first incomplete integration behavior fails.

- [x] **Step 4: Complete orchestration until the real workflow passes**

Fix only behavior exposed by the end-to-end test. Do not add automatic scheduling, JSON output, retention, layout restoration, or environment capture.

Run: `cargo test --all-targets`  
Expected: every unit and integration test passes with no warnings.

- [x] **Step 5: Run release and documentation verification**

Run:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
mdbook build docs
git diff --check
```

Expected: every command exits `0`, and `git status --short` lists only the intended uncommitted implementation and plan files.
