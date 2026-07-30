# Codex Unlinked Executable Recognition Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Classify a live Codex TUI whose native executable was unlinked by an upgrade while preserving every existing process, session, fallback, snapshot, and restore gate.

**Architecture:** Linux process observation will carry an opaque, crate-private command value containing the untouched `CapturedCommand` plus proof about the pinned executable inode. The inspector will acquire `/proc/<pid>/exe` through `O_PATH | O_CLOEXEC`, stabilize the raw link and `(st_dev, st_ino, st_nlink)`, and expose only a Codex-specific refined basename; all other classifiers and persisted commands continue to use the raw command.

**Tech Stack:** Rust 2024, Linux procfs, `libc`, `serde`, integration tests with `tempfile`, Cargo.

## Global Constraints

- Preserve the public raw-command constructors and accessors; callers outside the crate cannot forge unlinked-executable proof.
- Preserve `CapturedCommand` and all serialized snapshot bytes; add no snapshot field or schema version.
- Only Codex classification consumes the refined executable basename. Claude, `mdbook`/`book serve`, idle-shell checks, diagnostics, and manual fallback consume the untouched raw command.
- A Node/Bun/npm/pnpm wrapper alone never identifies Codex.
- Preserve exact foreground holder PID, rooted process tree, pane tty/cwd, opened session file identity, root `session_meta`, UUID, and distinct-file cardinality gates.
- Restore remains `codex resume <session-id>` using the currently installed and independently validated Codex executable.
- A positive link count never permits suffix stripping. Zero links require exactly one terminal ASCII ` (deleted)` decoration and a nonempty refined path.
- Procfs acquisition/refinement failure is `ProcessInspectionFailure` and therefore `PaneRecovery::Unavailable`; successful observation without complete Codex proof remains manual.
- Keep the change surgical to `src/process.rs`, `src/recovery.rs`, `tests/process_linux.rs`, and focused `tests/recovery.rs` additions.
- Do not push the branch.

---

### Task 1: Production-Inspector Regression And Proof-Bearing Observation

**Files:**
- Modify: `tests/process_linux.rs`
- Modify: `src/process.rs`
- Modify: `src/recovery.rs`

**Interfaces:**
- Consumes: existing `LinuxProcessInspector::with_proc_root_and_tool_stores`, `PaneProcessProbe::observe`, and `classify_pane`.
- Produces: a kernel-backed fake-proc regression named `recognizes_codex_when_the_native_executable_is_held_open_after_unlink`, crate-private `ObservedProcessCommand`, pinned acquisition in `LinuxProcessInspector`, and crate-private observed-evidence constructors.

- [ ] **Step 1: Add the held-file fake-proc fixture**

Open a temporary file whose basename is `codex`, retain the `File`, unlink its pathname, and point the fake native process's `exe` symlink at `/proc/self/fd/<held-fd>`. Keep the existing Node group leader, native child, exact cwd, and opened rollout record:

```rust
let native_path = temp.path().join("codex");
fs::write(&native_path, b"held native image").unwrap();
let native = File::open(&native_path).unwrap();
fs::remove_file(&native_path).unwrap();
let held_path = format!("/proc/self/fd/{}", native.as_raw_fd());

fake_process(
    &proc_root,
    201,
    &stat(201, "codex", 200, 200, 77, 34_817, 200, 21),
    &held_path,
    b"codex\0",
    "/tmp/work",
);
```

Assert the fixture's direct `read_link(proc_root.join("201/exe"))` is the held-FD path so the test cannot accidentally rely on a normalized fixture.

- [ ] **Step 2: Assert the consumer-visible result**

Observe with the production inspector, classify the returned evidence, and assert:

```rust
let PaneRecovery::Automatic(recovery @ AutomaticRecovery::Codex { session_id: id, .. }) =
    classify_pane(*evidence).recovery()
else {
    panic!("expected automatic Codex recovery");
};
assert_eq!(id.as_uuid().to_string(), session_id);
assert_eq!(
    derive_automatic_command(recovery)
        .argv()
        .iter()
        .map(LosslessOsString::as_bytes)
        .collect::<Vec<_>>(),
    vec![b"codex".as_slice(), b"resume".as_slice(), session_id.as_bytes()],
);
```

The production change this test catches is replacing pinned executable identity with raw `/proc/<pid>/exe` link text, which would restore the Node manual fallback.

- [ ] **Step 3: Run the exact RED test**

Run:

```bash
cargo test --test process_linux recognizes_codex_when_the_native_executable_is_held_open_after_unlink -- --exact --nocapture
```

Expected: FAIL because current `read_link` captures `/proc/self/fd/<n>` as the native executable and the classifier returns the raw Node leader as manual recovery.

- [ ] **Step 4: Add compile-failing private observation tests**

Inside `src/process.rs`, add module-private tests for a wished-for refinement function:

```rust
#[test]
fn zero_link_codex_strips_one_kernel_decoration_for_identity_only() {
    let command = observed_command(
        b"/tmp/codex (deleted)",
        8,
        42,
        0,
    )
    .unwrap();
    assert_eq!(command.command().executable().as_bytes(), b"/tmp/codex (deleted)");
    assert_eq!(command.executable_identity_basename(), Some(b"codex".as_slice()));
}
```

Add the parallel linked-`codex` case. Run the two tests and verify they fail to compile because `ObservedProcessCommand`/refinement do not yet exist.

Define the test helper directly above those cases; it must exercise the real private constructor rather than duplicate refinement logic:

```rust
fn observed_command(
    raw_link: &[u8],
    device: u64,
    inode: u64,
    link_count: u64,
) -> Result<ObservedProcessCommand, ProcessInspectionFailure> {
    ObservedProcessCommand::from_pinned(
        PinnedExecutableObservation {
            key: ProcessExecutableKey { device, inode },
            link_count,
            raw_link: LosslessOsString::try_from_bytes(raw_link.to_vec()).unwrap(),
        },
        vec![LosslessOsString::try_from_bytes(b"codex".to_vec()).unwrap()],
    )
}
```

- [ ] **Step 5: Add the proof-bearing types in `src/process.rs`**

Implement this crate-private shape, keeping pinned construction private to the module:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObservedProcessCommand {
    command: CapturedCommand,
    executable: ObservedExecutable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessExecutableKey {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ObservedExecutable {
    UnpinnedRaw,
    PinnedLinked {
        key: ProcessExecutableKey,
        link_count: NonZeroU64,
    },
    PinnedUnlinked {
        key: ProcessExecutableKey,
        identity_path: LosslessOsString,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PinnedExecutableObservation {
    key: ProcessExecutableKey,
    link_count: u64,
    raw_link: LosslessOsString,
}
```

Provide only these downstream operations:

```rust
pub(crate) fn unpinned(command: CapturedCommand) -> Self;
pub(crate) fn command(&self) -> &CapturedCommand;
pub(crate) fn executable_identity_basename(&self) -> Option<&[u8]>;
```

The exact private constructor is:

```rust
fn from_pinned(
    executable: PinnedExecutableObservation,
    argv: Vec<LosslessOsString>,
) -> Result<Self, ProcessInspectionFailure>;
```

It receives one stabilized `PinnedExecutableObservation` and constructs the raw `CapturedCommand` from that observation's `raw_link` plus `argv`, so raw executable text and inode proof cannot be mismatched. For `nlink > 0`, identity is the raw executable. For `nlink == 0`, strip exactly one terminal `b" (deleted)"`, reject a missing suffix or empty result with `ProcessInspectionFailure::InvalidEvidence`, and retain the raw executable in `CapturedCommand`.

- [ ] **Step 6: Acquire one pinned executable sample**

Add a private helper called from `LinuxProcessInspector`:

```rust
fn read_pinned_executable(
    executable_link: &Path,
    process_id: u32,
) -> Result<PinnedExecutableObservation, ProcessInspectionFailure>;
```

It must:

1. Open `executable_link` with `O_PATH | O_CLOEXEC`.
2. Read `File::metadata()` before the link read.
3. Read `/proc/self/fd/<fd>` losslessly.
4. Read `File::metadata()` again.
5. Require equal `(dev, ino, nlink)`; otherwise return `ObservationRaced { process_id }`.

Map open/read/stat failures to `ProcessInspectionFailure::Io` without file contents or environment data.

- [ ] **Step 7: Fence each process sample around argv and cwd**

In `read_process_once`, acquire the pinned executable before reading `cmdline`/cwd and acquire it again afterward. Require equality of key, link count, and raw link, then build `ObservedProcessCommand` from the second acquisition. Keep the existing stat identity/job check and the outer two-pass `read_process_stably` equality fence.

Change `InspectedProcess.command` to `ObservedProcessCommand`, so `same_observation` compares raw command plus the pinned proof.

- [ ] **Step 8: Carry observed commands through foreground evidence**

In `src/recovery.rs`, store `ObservedProcessCommand` privately in `ForegroundProcessMember` and `PaneTiedForegroundEvidence`. Preserve public constructors by wrapping their `CapturedCommand` as `ObservedProcessCommand::unpinned`. Add crate-private constructors accepting already observed commands for `LinuxProcessInspector` with these exact signatures:

```rust
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_new_observed(
    process_id: u32,
    parent_process_id: u32,
    process_group: u32,
    process_start_time: u64,
    process_tty: LosslessOsString,
    command: ObservedProcessCommand,
    working_directory: RecordedAbsolutePath,
) -> Result<Self, ForegroundEvidenceError>;

#[allow(clippy::too_many_arguments)]
pub(crate) fn try_new_observed(
    command: ObservedProcessCommand,
    pane_working_directory: RecordedAbsolutePath,
    pane_tty: LosslessOsString,
    process_tty: LosslessOsString,
    foreground_process_group: u32,
    process_id: u32,
    process_group: u32,
    process_start_time: u64,
) -> Result<Self, ForegroundEvidenceError>;
```

The first signature belongs to `ForegroundProcessMember`; the second belongs to `PaneTiedForegroundEvidence`. Their public `try_new` methods delegate to these after `ObservedProcessCommand::unpinned(command)`.

Keep public accessors exact:

```rust
pub fn command(&self) -> &CapturedCommand {
    self.command.command()
}
```

Build evidence from the crate-private constructors in `src/process.rs`. Preserve a raw iterator for Claude and add a separate observed iterator for Codex:

```rust
fn process_commands(&self) -> impl Iterator<Item = (u32, &CapturedCommand)>;
fn observed_process_commands(
    &self,
) -> impl Iterator<Item = (u32, &ObservedProcessCommand)>;
```

Idle-shell checks, default-shell comparison, and manual fallback must explicitly call `.command()` and therefore use raw values.

- [ ] **Step 9: Refine only the Codex predicate**

Make Codex resolution iterate observed commands and use:

```rust
fn is_codex_tui(command: &ObservedProcessCommand) -> bool {
    command.executable_identity_basename() == Some(b"codex")
        && command
            .command()
            .argv()
            .first()
            .and_then(|value| basename(value.as_bytes()))
            == Some(b"codex")
        && !command.command().argv()[1..].iter().any(|argument| {
            matches!(argument.as_bytes(), b"app-server" | b"exec" | b"mcp-server")
        })
}
```

Keep Claude and serve resolution on iterators of raw `CapturedCommand` values.

- [ ] **Step 10: Repair only fake-proc targets affected by `O_PATH`**

Update fake process fixtures so each `exe` link resolves to an existing file. Use `/usr/bin/zsh` for shell fixtures and matching initial-shell expectations. For synthetic Codex and Claude identities, create real temporary fixture files at paths ending in `codex` and `claude/versions/<version>` instead of dangling `/opt/...` paths. Do not weaken any classification assertion.

- [ ] **Step 11: Run GREEN checks**

Run:

```bash
cargo test --lib process::observation_tests -- --nocapture
cargo test --test process_linux -- --nocapture
cargo test --test recovery -- --nocapture
```

Expected: all pass, including the Task 1 regression and unchanged raw manual fallback assertions.

- [ ] **Step 12: Commit the minimal implementation**

```bash
git add src/process.rs src/recovery.rs tests/process_linux.rs
git commit -m "fix: recognize pinned unlinked Codex executables"
```

### Task 2: Fail-Closed Identity And Fallback Coverage

**Files:**
- Modify: `src/process.rs`
- Modify: `tests/process_linux.rs`
- Modify: `tests/recovery.rs`

**Interfaces:**
- Consumes: Task 1's private refinement/acquisition and unchanged public raw-evidence constructors.
- Produces: negative coverage for literal suffixes, malformed proof, noninteractive modes, positive link-count changes, and raw fallback.

- [ ] **Step 1: Add table-driven private refinement tests before any correction**

Add literal expected values covering:

```text
linked /tmp/codex                         -> codex
linked /tmp/codex (deleted)               -> codex (deleted)
unlinked /tmp/codex (deleted)             -> codex
unlinked /tmp/codex (deleted) (deleted)   -> codex (deleted)
unlinked /tmp/codex                       -> InvalidEvidence
unlinked " (deleted)"                     -> InvalidEvidence
```

Also assert raw executable bytes remain unchanged in every successful case. Run the exact tests before changing refinement and confirm any uncovered case fails for the expected branch.

- [ ] **Step 2: Add public raw-suffix rejection in `tests/recovery.rs`**

Construct public foreground evidence with executable `/tmp/codex (deleted)`, argv `codex`, and one otherwise valid exact root session. Assert it remains `PaneRecovery::Manual` and that the manual executable still contains the suffix. This proves public callers cannot forge unlink proof.

- [ ] **Step 3: Add production-inspector negative cases**

Using held-file fake proc fixtures, assert:

- an unlinked literal `codex (deleted)` produces two suffixes and remains manual;
- each of `app-server`, `exec`, and `mcp-server` remains manual even with proved unlink and exact session evidence;
- the raw Node group leader is the byte-exact manual fallback when Codex proof is otherwise insufficient.

The expected manual command is always the foreground leader, never the candidate member.

- [ ] **Step 4: Cover link-count and sampled-observation races**

Add module-private tests showing `ObservedProcessCommand` equality changes when only the positive `NonZeroU64` link count changes, and when only raw link or `(device, inode)` changes. Assert the existing `same_process_observations` fence rejects each mutation.

- [ ] **Step 5: Run the focused edge suite**

```bash
cargo test --lib process::observation_tests -- --nocapture
cargo test --test process_linux -- --nocapture
cargo test --test recovery -- --nocapture
```

Expected: all pass with raw diagnostics/fallback and every existing exact identity gate intact.

- [ ] **Step 6: Commit the fail-closed coverage**

```bash
git add src/process.rs tests/process_linux.rs tests/recovery.rs
git commit -m "test: cover unlinked executable identity boundaries"
```

### Task 3: Real Linux Acquisition And Final Verification

**Files:**
- Modify: `src/process.rs`
- Modify: `tests/process_linux.rs` only if the kernel helper belongs in integration coverage

**Interfaces:**
- Consumes: production `read_pinned_executable` and refinement from Task 1.
- Produces: a Linux kernel-backed unlink test plus complete repository verification.

- [ ] **Step 1: Add a real executable-unlink test**

In the private `src/process.rs` test module, copy `/bin/sleep` to a temporary path named `codex`, spawn that exact copy with argument `30`, and poll `/proc/<child>/exe` until its raw link equals the temporary path. Wrap the `Child` immediately in a test-only guard whose `Drop` kills and waits for it. Remove the temporary executable, call the production acquisition helper on `/proc/<child>/exe`, and assert:

```text
raw executable ends with /codex (deleted)
device and inode match the pre-unlink executable
link count is zero
refined identity basename is codex
```

Use a bounded condition poll rather than a fixed sleep. Fail with the last observed link or I/O error if the child does not reach the expected executable before the deadline.

- [ ] **Step 2: Run the kernel-backed test repeatedly**

```bash
for run in 1 2 3 4 5; do
  cargo test --lib process::observation_tests::production_acquisition_refines_a_running_unlinked_executable -- --exact --nocapture || exit 1
done
```

Expected: five clean passes with no remaining child process or temporary executable.

- [ ] **Step 3: Run formatting and strict static checks**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
```

Expected: exit 0 with no warnings.

- [ ] **Step 4: Run the complete serial suite**

```bash
cargo test --all-targets --all-features --locked -- --test-threads=1
```

Expected: every unit, integration, real tmux, capture, inspect, planning, and restore test passes.

- [ ] **Step 5: Verify the exact scope and bytes**

```bash
git diff --check
git status --short
git diff --stat 35d0550..HEAD
git diff -- src/model.rs src/inspect.rs src/restore.rs src/storage.rs
```

Expected: no whitespace errors; only the plan, `src/process.rs`, `src/recovery.rs`, `tests/process_linux.rs`, and focused `tests/recovery.rs` changes; no diff in snapshot/model/inspect/restore/storage code.

- [ ] **Step 6: Commit final kernel-test plumbing when the worktree is dirty**

```bash
git add src/process.rs tests/process_linux.rs
git commit -m "test: verify kernel-backed unlinked executable acquisition"
```

Run `git status --short`. Create this commit only when Step 1 produced an uncommitted `src/process.rs` or `tests/process_linux.rs` change; otherwise record that Task 2 already contains the kernel test and leave the clean worktree unchanged.
