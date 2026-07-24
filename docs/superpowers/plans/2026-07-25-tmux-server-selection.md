# tmux Server Selection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add root-level, tmux-style `-L` and `-S` selection to `snapshot` and `restore` while treating the selector as an opaque tmux argument and retaining one global snapshot stream.

**Architecture:** Parse the two CLI spellings once into one lossless `TmuxSelector`, store that selector unchanged in either the source adapter or a `RestoreDestination`, and use one command-construction boundary to append it to tmux commands. Restore planning never contacts the destination; execution establishes ownership through the existing token/PID/start-time protocol before any topology mutation.

**Tech Stack:** Rust 2024, Clap 4, tmux 3.4, `tempfile`, mdBook 0.5.3, mdbook-mermaid 0.17.0

## Global Constraints

- The authority for behavior is `docs/superpowers/specs/2026-07-24-tmux-server-selection-design.md`.
- The supported grammar is exactly:

  ```text
  tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] snapshot
  tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] restore [SNAPSHOT] [--run]
  ```

- `-L` and `-S` are mutually exclusive, non-repeatable, root-level options. They are rejected after a subcommand and with `inspect`. There are no long aliases.
- Remove `--target` without a compatibility test whose purpose is to prove its rejection.
- Preserve selector values as raw `OsString`. Do not make `-S` absolute, canonicalize either variant, derive a path from `-L`, inspect `TMUX_TMPDIR` or a UID, prepare selector-derived directories, or reproduce tmux validation.
- Do not add a selector-resolution error category. After parsing, report tmux rejection in the surrounding snapshot or restore operation.
- A selector type must make the two variants exclusive. Downstream code must not carry parallel `Option<OsString>` values or recheck exclusivity.
- One command-construction function appends the exact flag and raw value as two `Command` arguments. Execution must never reconstruct an argument from diagnostic rendering.
- An explicit source keeps its exact selector for every source command. Only ambient source discovery may observe `#{socket_path}` and pin subsequent commands as generated `-S`.
- Snapshot storage remains one global `snapshots/` directory and one global `latest`; selector data does not enter storage keys or layout.
- `RestorePlan` owns one `RestoreDestination`, which owns one selector. It has no duplicate resolved target path and no plan-time vacancy capability.
- With no explicit restore selector, planning generates `SocketPath(snapshot.source.path)` from validated source provenance.
- Plan-only restore makes no destination tmux call and no application filesystem write, and makes no vacancy, absence, availability, or resolution claim.
- `--run` prints the same plan first. Its initial claim is the only target command allowed to start a server; confirmation, mutation, verification, cleanup, and rollback all use no-start mode and the same selector.
- Ownership requires the attempt token, server PID, tmux server start time, OS process start time, and zero sessions. Never derive or compare an expected socket path from the selector.
- A failed claim returns no `OwnedRestoreTarget`. Cleanup after a possible start may mutate only through a narrower evidence type proving this attempt's token, PID, tmux start time, and OS process start time still match.
- An existing server reached through either selector must retain its process, sessions/topology, and options after the token mismatch.
- Preserve snapshot schema, restore status/result shapes, progress streams, recovery outcomes, and the program-recovery whitelist.
- Do not modify `docs/src/TOOL-RECOVERIES.md` or historical implementation plans.
- Follow strict TDD in every production slice: add or change a behavioral test, run it and record the expected failure, implement the minimum code, rerun the focused test, then run the adjacent suite.
- Keep changes surgical. Do not refactor unrelated inspect, recovery, storage, or rendering behavior.

---

### Task 1: Introduce the opaque selector module

**Files:**

- Create: `src/selector.rs`
- Modify: `src/lib.rs`

- [x] **Step 1: Write failing unit tests for exact argument emission**

  Add `src/selector.rs` with tests that construct a `std::process::Command`, append a selector, and inspect `Command::get_args()` as `OsStr` values. Register `mod selector;` in `src/lib.rs` in this RED step so Cargo discovers and compiles the new tests. Cover:

  ```rust
  TmuxSelector::SocketName(OsString::from("work"))
  TmuxSelector::SocketPath(OsString::from("./work.sock"))
  TmuxSelector::SocketName(OsString::new())
  TmuxSelector::SocketPath(OsString::from_vec(vec![b'.', b'/', 0xff]))
  ```

  Each test must assert exactly two appended arguments and byte-for-byte preservation. Add a mutation-sensitive assertion that `SocketName` emits `-L` and `SocketPath` emits `-S`.

- [x] **Step 2: Run the selector tests and confirm RED**

  Run:

  ```bash
  cargo test --locked --lib selector::tests
  ```

  Expected failure: unresolved `TmuxSelector` and selector-emission API.

- [x] **Step 3: Implement one deep selector type**

  Implement this public representation and its single command boundary:

  ```rust
  #[derive(Clone, Debug, Eq, PartialEq)]
  pub enum TmuxSelector {
      SocketName(OsString),
      SocketPath(OsString),
  }

  impl TmuxSelector {
      pub fn flag(&self) -> &'static str;
      pub fn value(&self) -> &OsStr;
      pub(crate) fn append_to(&self, command: &mut std::process::Command);
  }
  ```

  `append_to` must call `command.arg(self.flag()).arg(self.value())`. It must not convert through `String`, `Path`, display text, or a shell. Export the module from `src/lib.rs` using the repository's existing module/re-export style.

- [x] **Step 4: Run focused and library tests**

  Run:

  ```bash
  cargo test --locked --lib selector::tests
  cargo test --locked --lib
  ```

- [x] **Step 5: Commit the selector primitive**

  ```bash
  git add src/selector.rs src/lib.rs
  git commit -m "feat: add opaque tmux selector"
  ```

---

### Task 2: Migrate the CLI, source adapter, and restore core as one compiling slice

**Files:**

- Modify: `src/cli.rs`
- Modify: `src/main.rs`
- Modify: `src/restore.rs`
- Modify: `src/tmux.rs`
- Modify: `tests/cli.rs`
- Modify: `tests/restore_plan.rs`
- Modify: `tests/restore_execute.rs`
- Modify: `tests/tmux_source.rs`
- Modify: `tests/tmux_target.rs`

This is intentionally one vertical slice. Removing `TmuxServerIdentity` or changing `plan_restore` without adapting the binary leaves an uncompilable intermediate commit; a temporary compatibility layer would add exactly the duplicate target meaning this design removes.

- [ ] **Step 1: Rewrite restore-plan tests around `RestoreDestination`**

  Replace path-vacancy fixtures with selector fixtures and assert:

  - explicit `SocketName("abc")` is independent of snapshot provenance;
  - explicit relative `SocketPath("./rescue.sock")` remains unchanged;
  - no selector produces `SocketPath(snapshot.source().path())`;
  - rendering begins with exactly `target: -L abc`, `target: -S ./rescue.sock`, or `target: -S /recorded/source.sock`;
  - rendered plans contain no `vacant`, `absent`, `available`, `resolved`, or `target vacancy` field;
  - arbitrary selector bytes use the existing safe lossless diagnostic escaping.

  Delete planning tests whose only behavior was probing an existing/refused socket. Do not replace them with a deleted-`--target` compatibility test.

- [ ] **Step 2: Rewrite fake-executor tests so claim is the first target action**

  Change the fake `RestoreEnvironment` to expose only shell/home/directory/executable facts. Change the fake target capability so its first action is:

  ```rust
  claim(&RestoreDestination, &TargetShell)
  ```

  Remove scripted `recheck` outcomes and tests for pre-claim `TargetExists` or `TargetIndeterminate`. Retain and adapt tests for:

  - failure before `start-server` can begin -> `NotEstablished`;
  - failure after it may have run -> the supplied `Observed` disposition;
  - claim failure produces no topology or recovery call;
  - successful claim still gates topology, recovery, and rollback.

- [ ] **Step 3: Add failing parser and dispatch tests**

  Cover no selector, one `-L`, and one `-S` before both `snapshot` and `restore`. Assert rejection for mixed selectors in both orders, repeated `-L`, repeated `-S`, selectors after either supported subcommand, selectors with `inspect`, and missing selector arguments.

  Pass a Unix non-UTF-8 `OsString` through `Cli::try_parse_from` and assert byte equality in the refined request. Preserve selector-free inspect tests. Do not add a test focused on removal of `--target`.

- [ ] **Step 4: Add failing source and target command tests**

  Source tests must prove:

  - ambient discovery has no selector, records `#{socket_path}`, and uses generated `-S <reported-path>` later;
  - explicit `SocketName` and relative/non-UTF-8 `SocketPath` are attached unchanged to metadata and every topology query;
  - explicit source commands share one builder/context, including identical current directory and selector-relevant environment such as inherited `TMUX` handling;
  - ambient discovery's intentional context transition is limited to the first ambient query followed by generated `-S`;
  - source discovery retains `-N` and a failed selection publishes nothing.

  Target tests must prove the start-capable claim uses the exact selector without `-N`, every later client uses the exact selector with `-N`, and no command substitutes a reported socket path for an explicit selector.

  Do not assert tmux-rescue behavior for `TMUX_TMPDIR`, UID directories, canonicalization, or relative-path resolution.

- [ ] **Step 5: Run every changed suite and confirm RED**

  Run:

  ```bash
  cargo test --locked --bin tmux-rescue
  cargo test --locked --test cli
  cargo test --locked --test restore_plan
  cargo test --locked --test restore_execute
  cargo test --locked --test tmux_source -- --test-threads=1
  cargo test --locked --test tmux_target -- --test-threads=1
  ```

  Expected failures: the root parser has no selectors, source commands cannot retain an explicit selector, planning still probes an absolute target path, rendering still reports vacancy, and execution still rechecks before claim.

- [ ] **Step 6: Replace the path/vacancy model with a refined destination**

  In `src/restore.rs`, add:

  ```rust
  #[derive(Clone, Debug, Eq, PartialEq)]
  pub struct RestoreDestination {
      selector: TmuxSelector,
  }

  impl RestoreDestination {
      pub fn selector(&self) -> &TmuxSelector;
  }
  ```

  Change the planner boundary to:

  ```rust
  pub fn plan_restore(
      snapshot: &ValidatedSnapshot,
      explicit_selector: Option<TmuxSelector>,
      environment: &impl RestoreEnvironment,
  ) -> Result<RestorePlan, RestorePlanningError>;
  ```

  Refine `None` immediately into `TmuxSelector::SocketPath(snapshot.source().path().as_os_str().to_owned())`. Store only `destination: RestoreDestination` in `RestorePlan`; render and execute from that field.

  Remove `TmuxServerIdentity`, `TargetProbe`, `TargetVacancy`, `AvailableAtPlanning`, `ExecutionCheckedTarget`, `RestoreEnvironment::probe_target`, `SystemRestoreEnvironment::probe_target`, `probe_tmux_target`, the associated Unix-socket imports, and planning/execution errors that exist only for vacancy probing.

  Change the execution seam to:

  ```rust
  pub trait RestoreTargetCapability {
      fn claim(
          &mut self,
          destination: &RestoreDestination,
          shell: &TargetShell,
      ) -> Result<Box<dyn OwnedRestoreTarget>, TargetClaimFailure>;
  }
  ```

  `RestoreExecutor` calls `claim` directly. Preserve the existing owned-target, topology, recovery, rollback, result, and pane-outcome contracts.

- [ ] **Step 7: Parse root selectors into command-specific requests**

  Use a private Clap-derived `RawCli` with non-global root fields:

  ```rust
  #[arg(short = 'L', value_name = "SOCKET_NAME", conflicts_with = "socket_path")]
  socket_name: Option<OsString>,

  #[arg(short = 'S', value_name = "SOCKET_PATH", conflicts_with = "socket_name")]
  socket_path: Option<OsString>,
  ```

  Immediately refine the two raw optionals into one `Option<TmuxSelector>`. Add inherent `Cli::try_parse()` and `Cli::try_parse_from(...)` wrappers so `src/main.rs` retains Clap's existing error/exit handling. Reject selector plus `inspect` with `ErrorKind::ArgumentConflict`; do not mark selectors `global`, because that permits post-subcommand placement.

  Use:

  ```rust
  pub struct SnapshotRequest {
      pub selector: Option<TmuxSelector>,
  }

  pub struct RestoreRequest {
      pub snapshot: Option<PathBuf>,
      pub selector: Option<TmuxSelector>,
      pub run: bool,
  }
  ```

  Change `CliRunner::snapshot` to accept `SnapshotRequest`. Delete `RestoreTarget`, `RestoreTargetParser`, and absolute-path validation. Dispatch only forwards already-refined requests.

- [ ] **Step 8: Retain selectors through source and target command builders**

  Refine source discovery through:

  ```rust
  pub fn selected_source(
      selector: Option<TmuxSelector>,
  ) -> Result<TmuxAdapter<LinuxProcessInspector>, TmuxAdapterError>;
  ```

  Store validated `SnapshotSource` provenance and the operational selector. Explicit metadata and topology queries use the original selector. Ambient metadata uses no selector, then generates `SocketPath(reported_source.path)` for later queries only.

  Use a shared source-client constructor for an explicit endpoint so selector, current directory, and selector-relevant environment remain identical. Do not silently remove inherited `TMUX` on one explicit command but retain it on another. A test constructor receiving known provenance may generate `SocketPath(source.path)`; it is not a selector resolver.

  Use separate target constructors rather than a `may_start` boolean:

  ```rust
  fn start_capable_target_command(destination: &RestoreDestination) -> Command;
  fn no_start_target_command(destination: &RestoreDestination) -> Command;
  ```

  Both append through `TmuxSelector::append_to`; only the no-start builder adds `-N`. Keep target working directory and selector-relevant environment stable. Remove `socket_path` from ownership observations and all expected-path comparisons.

- [ ] **Step 9: Wire the binary without a write-capable plan-only dependency**

  Pass `SnapshotRequest.selector` to source discovery and `RestoreRequest.selector` to `plan_restore`. Snapshot loading remains independent: no path loads global `latest`, while a path loads that immutable snapshot.

  Factor preparation/rendering so the plan-only function receives only snapshot-loading inputs, `RestoreEnvironment`, and an output writer; it must have no target adapter, claim-config, temporary-file, or other write-capable dependency.

  Put target construction behind an internal injected factory closure. Add a binary unit test whose factory records construction and fails if invoked in plan-only mode. Add a `--run` ordering test that records plan flush before factory construction. The system entry point supplies `TmuxRestoreAdapter::new`; only its run branch invokes the factory.

- [ ] **Step 10: Run all affected targets and an all-target compile gate**

  Run:

  ```bash
  cargo test --locked --bin tmux-rescue
  cargo test --locked --test cli
  cargo test --locked --test restore_plan
  cargo test --locked --test restore_execute
  cargo test --locked --test tmux_source -- --nocapture --test-threads=1
  cargo test --locked --test tmux_target -- --nocapture --test-threads=1
  cargo test --locked --lib
  cargo check --locked --all-targets --all-features
  ```

  Inspect `rg -n -- '--target' src tests`; active code and tests must have no match, but do not add a removal-specific test.

- [ ] **Step 11: Commit the compiling vertical slice**

  ```bash
  git add src/cli.rs src/main.rs src/restore.rs src/tmux.rs tests/cli.rs tests/restore_plan.rs tests/restore_execute.rs tests/tmux_source.rs tests/tmux_target.rs
  git commit -m "feat: pass tmux selectors through snapshot and restore"
  ```

---

### Task 3: Strengthen selector-based ownership and cleanup

**Files:**

- Modify: `src/tmux.rs`
- Modify: `tests/tmux_target.rs`

- [ ] **Step 1: Add failing ownership-boundary tests**

  Add test coverage for each safety transition:

  - an existing `-L` server and an existing `-S` server reject the claim token and retain the same PID, sessions/topology, and sentinel options;
  - a spawn/dispatch failure before `start-server` may begin reports `NotEstablished`;
  - once `start-server` may have run, claim failure returns no owned capability and reports `Observed(Removed | Retained | Missing | Unknown)` from evidence;
  - failed-claim cleanup does not kill when token, PID, tmux start time, or OS process start time is missing or mismatched;
  - post-claim endpoint replacement prevents the next mutation;
  - rollback retains the exact selector and uses no-start mode.

  Make the existing-server tests table-driven over `SocketName` and `SocketPath` only where shared assertions remain readable.

- [ ] **Step 2: Run the complete target suite and confirm RED**

  Run the full file so every new safety family, including pre-start state and rollback-selector retention, participates in the RED gate:

  ```bash
  cargo test --locked --test tmux_target -- --nocapture --test-threads=1
  ```

  Expected failures: current failed-claim cleanup is token-only, ownership still relies on path identity, or a target command does not retain the exact selector/no-start behavior.

- [ ] **Step 3: Encode cleanup authorization as a narrower proof type**

  Keep `OwnedRestoreTarget` exclusive to a successful zero-session claim. Introduce a private cleanup-only capability, named for the proof it carries, that can be constructed only after reading and matching:

  ```text
  exact RestoreDestination selector
  attempt token
  server PID
  tmux server start time
  OS process start time
  ```

  A failed claim must never return `OwnedRestoreTarget`. If cleanup cannot construct the cleanup-only proof, or if the same server now has any session, it must not issue a mutating command and must report the conservative observed disposition.

  Make the claim attempt structurally one-shot once `start-server` may have been dispatched. Do not leave a reusable adapter state that can retry after an ambiguous attempt.

  Split process creation from waiting: failure to spawn the claim command reports `NotEstablished`; once `Command::spawn()` succeeds, any wait, exit-status, confirmation, or cleanup failure reports an evidence-based `Observed(...)` state because `start-server` may have run.

- [ ] **Step 4: Guard every post-claim mutation with the full identity**

  Retain the selector, token, PID, tmux start time, and OS process start time in the owned target. Before a mutating client command, verify the OS PID/start pair and use the existing atomic tmux guard for token/PID/tmux start time. If either proof fails, stop without issuing the mutation.

  Derive final `TargetDisposition` conservatively from same-selector readback plus OS process identity:

  - matching live identity -> `Retained`;
  - confirmed termination of the matched process -> `Removed` or `Missing` according to the existing terminal phase;
  - reachable mismatch or incomplete/failed evidence -> `Unknown` unless the existing result contract has stronger evidence.

  Do not inspect a selector-derived filesystem path to decide disposition.

- [ ] **Step 5: Run all target and execution tests**

  Run:

  ```bash
  cargo test --locked --test tmux_target -- --nocapture --test-threads=1
  cargo test --locked --test restore_execute
  cargo test --locked --lib tmux
  ```

- [ ] **Step 6: Commit the ownership hardening**

  ```bash
  git add src/tmux.rs tests/tmux_target.rs
  git commit -m "fix: preserve ownership proof across tmux selectors"
  ```

---

### Task 4: Prove the global stream and end-to-end behavior

**Files:**

- Modify: `tests/e2e.rs`
- Modify: `tests/storage.rs`
- Modify: `tests/tmux_source.rs`
- Modify: `tests/tmux_target.rs`

- [ ] **Step 1: Make the existing idle-pane fixture state-ready**

  Capture the initial pane ID from `new-session -P -F '#{pane_id}'`. Before snapshotting, poll a semantic condition until that pane's foreground process is the intended idle shell; require two consecutive matching observations so the login profile's transient `update-motd` cannot satisfy readiness.

  Reuse the production process observation seam where practical. Do not add an arbitrary sleep and do not weaken the expected idle-pane assertion.

- [ ] **Step 2: Add a two-source global-stream test**

  Start one isolated source with `-L` and another with `-S`, capture both through the binary with one `XDG_STATE_HOME`, and assert:

  - both immutable files have the same `snapshots/` parent;
  - the captures have distinct immutable paths;
  - the one global `latest` points to the later capture under existing ordering rules;
  - each snapshot records the socket path reported by tmux for its source.

  For test isolation only, give named-socket commands one temporary `TMUX_TMPDIR`. The production code and assertions must not derive or claim the resulting path.

- [ ] **Step 3: Add plan-only non-contact/non-write coverage**

  Retain Task 2's orchestration proof that plan-only preparation has no write-capable input and does not invoke the injected target factory.

  For the binary test, use an explicit snapshot and controlled temporary cwd, `TMPDIR`, `XDG_STATE_HOME`, `HOME`, and `TMUX_TMPDIR`. Inventory every controlled tree before and after plan-only restore to explicit `-L` and relative `-S` destinations. Assert byte-for-byte tree equality, exact `target:` output, and unchanged live target fingerprints where a target already exists.

  The test scope is tmux-rescue-managed state and destination effects; caller-owned stdout capture is not an application filesystem write.

- [ ] **Step 4: Update the comprehensive restore flow**

  Replace positive `--target` invocations with root-level `-S`. Assert the exact target line rather than a substring, keep plan-only/run plan parity, and preserve all existing topology and pane-recovery assertions.

  Add or retain real existing-server protection for both selector variants, including PID, full session/topology inventory, and sentinel global options before and after the failed claim.

- [ ] **Step 5: Run the real-tmux tests and confirm GREEN**

  Run serially:

  ```bash
  cargo test --locked --test tmux_source -- --nocapture --test-threads=1
  cargo test --locked --test tmux_target -- --nocapture --test-threads=1
  cargo test --locked --test e2e -- --nocapture --test-threads=1
  cargo test --locked --test storage
  ```

  Repeat the e2e test once to verify the state-based readiness fix is stable.

- [ ] **Step 6: Commit integration evidence**

  ```bash
  git add tests/e2e.rs tests/storage.rs tests/tmux_source.rs tests/tmux_target.rs
  git commit -m "test: cover tmux selector integration"
  ```

---

### Task 5: Synchronize public contracts and run release-grade verification

**Files:**

- Modify: `README.md`
- Modify: `docs/src/DESIGN.md`
- Modify: `docs/src/ARCHITECTURE.md`

- [ ] **Step 1: Update README from the user workflow down**

  Show ambient, `-L`, and `-S` snapshot examples and root-level restore examples. State that all captures share one archive and one global `latest`, snapshot choice and destination choice are independent, plan-only does not contact the destination, the exact selector is printed, and `--run` attempts ownership without mutating an existing server.

- [ ] **Step 2: Update the design overview**

  In `docs/src/DESIGN.md`, describe the command grammar, opaque pass-through, ambient-versus-explicit source flow, one global stream, independent restore selection, generated source-path fallback, plan-only non-contact/non-write behavior, and ownership claim before topology.

- [ ] **Step 3: Update the architecture contract**

  In `docs/src/ARCHITECTURE.md`:

  - define `TmuxSelector` and `RestoreDestination`;
  - remove the resolved-path/vacancy/refined-preflight model;
  - define the single raw-argument command boundary and selector lifetime;
  - document ambient pinning and explicit selector retention;
  - state that storage has no selector partition;
  - document exact plan output and source-path fallback;
  - document the sole start-capable claim command, no-start post-claim commands, full ownership proof, cleanup-only evidence, and existing-server protection;
  - replace obsolete verification claims with the implemented selector/global-stream evidence.

  Keep `docs/src/TOOL-RECOVERIES.md`, `docs/src/SUMMARY.md`, and historical plans unchanged.

- [ ] **Step 4: Check documentation terms and build the book**

  Run:

  ```bash
  rg -n -- '--target|TargetVacancy|AvailableAtPlanning|ExecutionCheckedTarget' README.md docs/src src tests
  mdbook build docs
  ```

  Expected `rg` result: no matches in active source, tests, README, DESIGN, or ARCHITECTURE.

- [ ] **Step 5: Run all repository gates**

  Run:

  ```bash
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features --locked -- -D warnings
  cargo test --all-targets --all-features --locked -- --test-threads=1
  cargo build --release --locked
  cargo doc --all-features --locked --no-deps
  mdbook build docs
  cargo package --list --locked
  cargo package --locked
  ```

  Run the deployable Mermaid asset guard from `.github/workflows/docs.yml` verbatim after the book build.

- [ ] **Step 6: Inspect the final diff for scope and contract completeness**

  Run:

  ```bash
  git status --short
  git diff --check
  git diff --stat origin/main
  rg -n -- '--target' src tests README.md docs/src
  ```

  Confirm every changed line serves server selection, global-stream evidence, the fixture readiness repair, or required documentation synchronization.

- [ ] **Step 7: Commit documentation and any verification-only fixes**

  ```bash
  git add README.md docs/src/DESIGN.md docs/src/ARCHITECTURE.md
  git commit -m "docs: document tmux server selection"
  ```
