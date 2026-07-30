# Codex Visible Prompt Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Capture the visible unsent Codex 0.145.0 prompt area in an immutable snapshot and, after restoring the exact recorded Codex session, bracketed-paste that text back into its composer without pressing Enter.

**Architecture:** Refine persisted prompt text inside only the Codex automatic-recovery variant. A stable tmux grid adapter parses ephemeral pane identity, geometry, cursor, mode, and exact visible rows; a private pure Codex parser turns only the supported bottom-pane grammar into prompt text. Restore planning binds that text to an opaque matching Codex launch, and the target performs a fresh exact-session observation plus guarded literal paste as one operation.

**Tech Stack:** Rust 2024, tmux 3.4, serde/serde_json, `unicode-width` 0.2, existing Linux `/proc` process evidence, mdBook 0.5.3

## Global Constraints

- The behavior authority is `docs/superpowers/specs/2026-07-25-codex-visible-prompt-recovery-design.md`.
- Capture is automatic only during an explicit `snapshot`; add no flag, timer, hook, crash watcher, or background persistence.
- Treat the visible terminal cells as authoritative. Preserve blank rows and indentation, turn each rendered row boundary into `\n`, retain `[Pasted Content N chars]` literally, and never claim hidden or scrolled-out composer state.
- Support only the normal Codex 0.145.0 composer grammar proved by the read-only `%15` fixture: pane `132x40`, accepted `»` prompt at row 33, cursor `(9,37)`, five prompt rows, blank unused textarea/inset rows below the cursor, and one two-cell-indented footer row. Official Codex 0.145.0 `empty`, `large`, and queue-footer renderer snapshots additionally prove that the count of unused blank textarea rows varies with draft height; accept one or more such rows but never include them in captured text. Tests may resize fixtures while retaining these structural relationships.
- Recognize only `›` and `»`; reject shell mode `!`. Require the cursor on the final prompt row at its visible end. Fail closed for popups, copy mode, changing metadata, unknown prefixes, unknown footer shapes, or ambiguous trimmed trailing spaces.
- Use terminal display-cell width for prefixes and cursor alignment. Do not use UTF-8 byte length or Unicode scalar count as a substitute.
- `CapturedPromptText` must prove non-whitespace content, newline-only controls, no CR/ESC/C1 controls, and a maximum UTF-8 size of 16 KiB. Derive visible row and byte counts from the value; do not serialize duplicate counts.
- Nest prompt input only in `AutomaticRecovery::Codex`. Old snapshots must deserialize with `None`; new snapshots omit the field when absent. Existing old binaries may reject the new field under their current unknown-field policy.
- Prompt capture is optional enrichment. `Absent` is silent. A read or parser failure emits a bounded terminal-safe warning without prompt text and preserves otherwise valid Codex session recovery.
- Prompt enrichment must not make an otherwise valid candidate exceed `MAX_SNAPSHOT_BYTES`. If the enriched serialized candidate crosses that existing aggregate limit, remove all optional prompt areas from that candidate, emit a count-free safe skip event for each removed area, and validate/publish the same session-recovery candidate without them. Do not add a second persisted budget or change the snapshot size limit.
- Inspection and restore plans show only row/byte counts. Never echo prompt text outside owner-only snapshot JSON.
- Automatic fallback, failed session recovery, an unexpected session, a missing pane, or lost target ownership must send no prompt text. Never paste into the fallback shell.
- After normal settle verifies automatic recovery, re-observe the exact Codex session immediately before paste. The target operation must own this fresh observation and the paste; do not expose a reusable verified token.
- Prompt preparation uses only `set-buffer` followed by `paste-buffer -d -p -r`. It must never issue `send-keys Enter`, retry automatically, or reuse the automatic-settle observation.
- Retain existing snapshot immutability, owner-only permissions, one global `latest`, fresh-target restore, selector behavior, automatic-recovery whitelist, topology behavior, and non-Codex recovery behavior.
- Every production slice follows strict TDD: add a focused behavioral test, run it and observe the expected RED failure, implement the minimum behavior, rerun the focused test, then run the adjacent suite.
- Keep commits surgical and task-local. Do not refactor unrelated capture, recovery, inspect, restore, tmux selection, storage, or presentation code.

## File Map

- `Cargo.toml`, `Cargo.lock`: add `unicode-width` for terminal-cell comparisons.
- `src/model.rs`: own persisted raw/validated prompt values, invariants, compatibility, getters, and count derivation.
- `src/visible_pane.rs`: own parsed pane ID, geometry, cursor/mode metadata, rows, and exact-height grid construction.
- `src/codex_prompt.rs`: privately own the supported Codex screen grammar and `Absent | Captured | Skipped` result.
- `src/lib.rs`: register the visible-grid module publicly for capture capability implementations and the parser privately.
- `src/capture.rs`: carry ephemeral pane IDs in topology, request grids only for exact Codex panes, attach prompt enrichment, and report privacy-safe failures.
- `src/tmux.rs`: perform stable source-grid reads and guarded post-recovery literal paste after fresh exact-session classification.
- `src/recovery.rs`: ignore prompt enrichment when deriving and comparing the existing automatic recovery identity/command.
- `src/inspect.rs`: render optional pending-input counts for Codex panes.
- `src/restore.rs`: bind prompt input to an opaque Codex launch, execute post-recovery preparation, and type the new outcomes.
- `src/cli.rs`: render capture warnings and restore outcomes without prompt text.
- `tests/model.rs`, `tests/capture.rs`, `tests/tmux_source.rs`: cover schema, refined grids, gating, stability, and capture enrichment.
- `tests/restore_plan.rs`, `tests/restore_execute.rs`, `tests/tmux_target.rs`, `tests/cli.rs`: cover opaque binding, fresh verification, literal paste mechanics, outcomes, and privacy.
- `tests/recovery.rs`, `tests/process_linux.rs`, `tests/e2e.rs`: synchronize Codex fixtures and prove existing recovery behavior remains unchanged.
- `README.md`, `docs/src/DESIGN.md`, `docs/src/ARCHITECTURE.md`, `docs/src/TOOL-RECOVERIES.md`: synchronize user contract, domain boundaries, parser evidence, limits, privacy, and restore safety.

---

### Task 1: Refine and persist visible Codex prompt text

**Files:**

- Modify: `src/model.rs`
- Modify: `src/recovery.rs`
- Modify: `tests/model.rs`
- Modify: `tests/recovery.rs`
- Modify: `tests/process_linux.rs`
- Modify: fixture constructors in `src/cli.rs`, `src/inspect.rs`, `tests/cli.rs`, `tests/restore_plan.rs`, `tests/restore_execute.rs`, and `tests/tmux_target.rs`

**Interfaces:**

```rust
pub const MAX_CODEX_PROMPT_BYTES: usize = 16 * 1024;

pub struct CapturedPromptText(String);

impl CapturedPromptText {
    pub(crate) fn try_new(text: String) -> Result<Self, SnapshotValidationError>;
    pub fn as_str(&self) -> &str;
    pub fn visible_row_count(&self) -> usize;
    pub fn byte_count(&self) -> usize;
}

pub struct CapturedCodexPromptArea {
    text: CapturedPromptText,
}

impl CapturedCodexPromptArea {
    pub(crate) fn try_new(text: String) -> Result<Self, SnapshotValidationError>;
    pub fn text(&self) -> &CapturedPromptText;
}
```

- [ ] **Step 1: Add failing schema and refinement tests**

  In `tests/model.rs`, add:

  - `validates_and_round_trips_a_codex_visible_prompt_area`
  - `older_codex_snapshots_default_prompt_area_to_none_and_omit_it_on_write`
  - `rejects_whitespace_control_and_oversized_prompt_text`
  - `rejects_unknown_fields_inside_a_prompt_area`

  Use this exact raw shape and assert the validated value reports `5` visible rows and `49` UTF-8 bytes:

  ```json
  {
    "kind": "codex",
    "session_id": "018f8f15-2e24-7a8a-a5c0-bf32e04c45be",
    "prompt_area": {
      "text": "The test prompt for recovering.\n\nLine 1.\n\nLine 2."
    }
  }
  ```

  Rejection cases must include empty text, Unicode-only whitespace, `\r`, NUL, ESC, C1 U+0085, and `16 * 1024 + 1` ASCII bytes. Assert error diagnostics never include the rejected text.

- [ ] **Step 2: Run the model suite and confirm RED**

  ```bash
  cargo test --locked --test model
  ```

  Expected failure: `prompt_area` is an unknown field and the captured prompt types/getters do not exist.

- [ ] **Step 3: Add the raw and validated model**

  In `src/model.rs`, add a private raw object with the repository's existing strict object policy:

  ```rust
  #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
  #[serde(deny_unknown_fields)]
  pub struct RawCapturedCodexPromptArea {
      pub text: String,
  }
  ```

  Change both Codex variants to:

  ```rust
  RawAutomaticRecovery::Codex {
      session_id: String,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      prompt_area: Option<RawCapturedCodexPromptArea>,
  }

  AutomaticRecovery::Codex {
      session_id: CodexSessionId,
      prompt_area: Option<CapturedCodexPromptArea>,
  }
  ```

  Add `SnapshotValidationError::InvalidCodexPromptText { reason: String }`. The constructor must return the refined string, not validate and retain the raw representation. Permit `\n`; reject every other `char::is_control()` value. Count bytes with `String::len()` and rows with `split('\n').count()`.

  Update raw-to-validated and validated-to-raw conversions. Update all process/recovery constructors to set `prompt_area: None`, and add `..` only to matches that intentionally ignore enrichment. `AutomaticRecoveryExpectation` and `derive_automatic_command` continue to depend only on the session ID.

- [ ] **Step 4: Run focused and adjacent suites**

  ```bash
  cargo test --locked --test model
  cargo test --locked --test recovery
  cargo test --locked --test process_linux
  cargo test --locked --lib
  ```

- [ ] **Step 5: Commit the refined snapshot model**

  ```bash
  git add src/model.rs src/recovery.rs src/cli.rs src/inspect.rs tests/model.rs tests/recovery.rs tests/process_linux.rs tests/cli.rs tests/restore_plan.rs tests/restore_execute.rs tests/tmux_target.rs
  git commit -m "feat: model visible Codex prompt input"
  ```

---

### Task 2: Parse ephemeral pane identity and exact visible grids

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `src/visible_pane.rs`
- Modify: `src/lib.rs`

**Interfaces:**

```rust
pub struct TmuxPaneId(String);
pub struct PaneWidth(NonZeroU16);
pub struct PaneHeight(NonZeroU16);
pub struct VisibleCellPosition { x: u16, y: u16 }
pub struct VisiblePaneMetadata {
    pane_id: TmuxPaneId,
    width: PaneWidth,
    height: PaneHeight,
    cursor: VisibleCellPosition,
    in_mode: bool,
}
pub struct VisibleRow(String);
pub struct VisiblePaneGrid {
    metadata: VisiblePaneMetadata,
    rows: Vec<VisibleRow>,
}
```

- [ ] **Step 1: Add failing refined-grid tests**

  Run `cargo add unicode-width@0.2` so grid refinement and the later Codex parser share one terminal-cell rule. Create `src/visible_pane.rs` with the tests below and register `mod visible_pane; pub use visible_pane::*;` in `src/lib.rs` during this RED step, so Cargo discovers the new tests before the interfaces exist:

  - `%15`, nonzero `132x40`, cursor `(9,37)`, mode `0`, and exactly 40 newline-delimited rows;
  - malformed pane IDs, zero dimensions, out-of-range cursor cells/rows, malformed UTF-8, CR/control cells, a rendered row wider than the pane, absent final tmux row delimiter, and row-count mismatch;
  - a final empty row represented by a trailing empty field after removing exactly one output delimiter.

- [ ] **Step 2: Run and confirm RED**

  ```bash
  cargo test --locked --lib visible_pane::tests
  ```

  Expected failure: the visible-grid types and refined tmux pane ID do not exist.

- [ ] **Step 3: Implement refined grid construction**

  In the already registered module, parse IDs as `%` plus one or more ASCII digits. Parse dimensions into `NonZeroU16`. Require `cursor.x < width` and `cursor.y < height`; the parser will later compare the cursor insertion cell to rendered end positions.

  Give every refined component the construction/access boundary needed by sibling modules and integration fakes:

  ```rust
  impl TmuxPaneId {
      pub fn try_from_bytes(value: Vec<u8>) -> Result<Self, VisiblePaneGridError>;
      pub fn as_str(&self) -> &str;
  }

  impl PaneWidth { pub fn try_new(value: u16) -> Result<Self, VisiblePaneGridError>; pub fn get(&self) -> u16; }
  impl PaneHeight { pub fn try_new(value: u16) -> Result<Self, VisiblePaneGridError>; pub fn get(&self) -> u16; }

  impl VisiblePaneMetadata {
      pub fn try_new(
          pane_id: TmuxPaneId,
          width: u16,
          height: u16,
          cursor_x: u16,
          cursor_y: u16,
          in_mode: bool,
      ) -> Result<Self, VisiblePaneGridError>;
      pub fn pane_id(&self) -> &TmuxPaneId;
      pub fn width(&self) -> PaneWidth;
      pub fn height(&self) -> PaneHeight;
      pub fn cursor(&self) -> VisibleCellPosition;
      pub fn in_mode(&self) -> bool;
  }

  impl VisibleCellPosition { pub fn x(&self) -> u16; pub fn y(&self) -> u16; }
  impl VisibleRow { pub fn as_str(&self) -> &str; }
  ```

  Implement:

  ```rust
  impl VisiblePaneGrid {
      pub fn try_from_tmux_capture(
          metadata: VisiblePaneMetadata,
          output: Vec<u8>,
      ) -> Result<Self, VisiblePaneGridError>;
      pub fn metadata(&self) -> &VisiblePaneMetadata;
      pub fn rows(&self) -> &[VisibleRow];
  }
  ```

  Require valid UTF-8, remove exactly one final `\n`, split on `\n`, require exactly `height` rows, reject all controls including CR, and use `UnicodeWidthStr::width()` to reject a row wider than `PaneWidth`. Do not trim or join rows in this type.

- [ ] **Step 4: Run focused and capture tests**

  ```bash
  cargo test --locked --lib visible_pane::tests
  cargo test --locked --lib
  ```

- [ ] **Step 5: Commit pane/grid refinement**

  ```bash
  git add Cargo.toml Cargo.lock src/visible_pane.rs src/lib.rs
  git commit -m "feat: refine visible tmux pane grids"
  ```

---

### Task 3: Parse only the supported Codex prompt grammar

**Files:**

- Create: `src/codex_prompt.rs`
- Modify: `src/capture.rs`
- Modify: `src/lib.rs`

**Interfaces:**

```rust
pub(crate) enum CodexPromptAreaObservation {
    Absent,
    Captured(CapturedCodexPromptArea),
    Skipped(CodexPromptCaptureFailure),
}

pub struct CodexPromptCaptureFailure(CodexPromptCaptureFailureKind);

pub(crate) fn capture_visible_codex_prompt(
    grid: &VisiblePaneGrid,
) -> CodexPromptAreaObservation;
```

- [ ] **Step 1: Add failing parser fixtures**

  Register private `mod codex_prompt;`, define the public opaque `CodexPromptCaptureFailure` in `src/capture.rs`, and write inline tests using the `unicode-width` cell rule established in Task 2. Its private kind may distinguish read failure, metadata change, unsupported layout, unsafe text, and size overflow; expose `message()` plus a fallible public read-failure constructor so external `CaptureSource` fakes can implement the public trait without exposing the inner representation.

  - `captures_five_visible_rows_and_preserves_blank_lines`
  - `accepts_both_codex_prompt_glyphs`
  - `preserves_indentation_soft_wraps_and_a_trailing_empty_row`
  - `preserves_literal_pasted_content_placeholders`
  - `accepts_a_visible_scrolled_suffix_without_claiming_completeness`
  - `returns_absent_for_an_empty_composer`
  - `accepts_normal_variable_unused_textarea_rows`
  - `skips_shell_mode_popup_and_unrecognized_bottom_layouts`
  - `skips_a_cursor_in_the_middle_of_input`
  - `skips_a_continuation_without_the_two_cell_margin`
  - `skips_when_trimmed_trailing_spaces_break_cursor_alignment`
  - `skips_unsafe_and_oversized_prompt_text`

  The main positive grid must freeze the approved rows:

  ```text
  » The test prompt for recovering.

    Line 1.

    Line 2.

    gpt-5.6-sol ultra · ~/projects/tmux-rescue · main · Context 78% used · 258K window · Fast on · Approve for me · 2.55M used · Main…
  ```

  The grid has arbitrary transcript rows above this suffix. The cursor is `(9,37)` in a `132x40` pane. The expected captured string is the 49-byte five-row fixture from Task 1.

- [ ] **Step 2: Run parser tests and confirm RED**

  ```bash
  cargo test --locked --lib codex_prompt::tests
  ```

  Expected failure: parser observation/failure types and grammar logic are absent.

- [ ] **Step 3: Implement the narrow parser**

  Use `UnicodeWidthStr::width()` for every display-cell comparison. The accepted layout must satisfy all of these local proofs:

  1. `pane_in_mode == false` and height is at least three rows.
  2. `cursor.y <= height - 3`.
  3. every row after the cursor through `height - 2` is empty after tmux trimming, with at least one such row.
  4. row `height - 1` starts with at least two ASCII spaces and satisfies the supported one-line footer grammar below.
  5. scanning backward from the cursor row finds a start row whose first two cells are exactly `› ` or `» `.
  6. every later candidate row is either empty or begins with two ASCII spaces.
  7. the final nonempty row's rendered width equals `cursor.x`; an empty continuation row is accepted only at `cursor.x == 2`.

  Implement `is_supported_codex_0145_footer` without a general regular-expression dependency. After removing at least the two-cell footer indent and trimming, accept only:

  - a default contextual footer ending in an ASCII integer from `0` through `100` followed by `% context left`, whose optional left hint matches `base [" · " plan]`; `base` is empty, `? for shortcuts`, `tab to queue`, or `tab to queue message`, and `plan` is `Plan mode` or `Plan mode (shift+tab to cycle)`. Also accept either `plan` spelling by itself;
  - the same known shortcut/queue/plan hint without right context when narrow-width collapse removes the context; or
  - a configured one-line status footer split by ` · ` with at least one exact `Context N% used` segment where `N` is an ASCII integer from `0` through `100`. This accepts the approved live `Context 78% used` footer while retaining a renderer-specific anchor.

  Reject an arbitrary indented nonempty final row, unknown hint text, an out-of-range percentage, or a multirow footer/popup. Freeze positive fixtures for the approved configured footer, default `tab to queue message ... 98% context left`, empty-composer `? for shortcuts ... 100% context left`, and narrow queue collapse; freeze negative fixtures for an arbitrary `status`, unknown hints, malformed percentages, shortcut overlay, and popup rows.

  Remove exactly the glyph-plus-space from the first row and exactly two ASCII spaces from nonempty continuation rows. Map trimmed empty terminal rows to empty prompt lines. Join accepted rows with `\n` and finish through `CapturedCodexPromptArea::try_new`.

  Return `Absent` only when the accepted one-row candidate is exactly the Codex 0.145.0 normal placeholder `Ask Codex to do anything` and the cursor is at cell 2. Any other nonempty candidate with the cursor at cell 2 is `Skipped`, as is a cursor inside later text, layout ambiguity, unsafe text, or overflow. `CodexPromptCaptureFailure` must expose only bounded terminal-safe reason text, must never store captured rows, and must have constructors usable by the parser, external capture fakes, and tmux read adapter.

- [ ] **Step 4: Run parser and model tests**

  ```bash
  cargo test --locked --lib codex_prompt::tests
  cargo test --locked --test model
  cargo test --locked --lib
  ```

- [ ] **Step 5: Commit the pure parser**

  ```bash
  git add src/codex_prompt.rs src/capture.rs src/lib.rs
  git commit -m "feat: parse visible Codex prompt areas"
  ```

---

### Task 4: Capture a stable grid only for exact Codex panes

**Files:**

- Modify: `src/capture.rs`
- Modify: `src/tmux.rs`
- Modify: `src/cli.rs`
- Modify: `tests/capture.rs`
- Modify: `tests/process_linux.rs`
- Modify: `tests/tmux_source.rs`
- Modify: `tests/tmux_target.rs`

**Interfaces:**

```rust
pub trait CaptureSource {
    fn source(&self) -> &SnapshotSource;
    fn read_topology(&mut self) -> Result<TopologyObservation, CaptureSourceFailure>;
    fn inspect_pane(&mut self, pane: &TopologyPane) -> PaneProcessObservation;
    fn read_visible_pane(
        &mut self,
        pane: &TopologyPane,
    ) -> Result<VisiblePaneGrid, CodexPromptCaptureFailure>;
}

CaptureEvent::CodexPromptCaptureSkipped {
    attempt: usize,
    pane: SourcePaneCoordinate,
    failure: CodexPromptCaptureFailure,
}
```

- [ ] **Step 1: Add failing orchestration tests**

  Extend the fake source in `tests/capture.rs` with scripted visible-grid reads and a call log. Add:

  - `exact_codex_capture_attaches_visible_prompt_input`
  - `non_codex_and_downgraded_panes_never_read_the_visible_grid`
  - `absent_prompt_input_emits_no_event`
  - `skipped_prompt_input_retains_automatic_codex_recovery`
  - `prompt_failure_events_never_contain_prompt_text`
  - `topology_replacement_with_the_same_coordinate_retries_capture`
  - `snapshot_budget_pressure_drops_prompt_enrichment_without_discarding_codex_recovery`

  Assert a failed read/parser leaves `AutomaticRecovery::Codex { session_id, prompt_area: None }`, while a successful parse nests the exact validated area. Include the sensitive fixture in the source and assert it is absent from every event's `Debug`/`Display` rendering. For the replacement test, two observations with the same session/window/pane indexes but different `%N` IDs must trigger the existing retry path. Exercise the aggregate fallback through a private serialization-budget helper with a small test limit: an enriched candidate over the test limit must strip prompt fields and validate the same Codex session recovery, while a prompt-free candidate that is independently oversized must retain the existing fatal validation result.

- [ ] **Step 2: Add failing tmux command tests**

  In `tests/tmux_source.rs`, update source topology fixtures for the eleventh `#{pane_id}` field and add:

  - `visible_grid_capture_uses_stable_metadata_and_never_joins_rows`
  - `changing_metadata_or_wrong_row_count_skips_prompt_capture`
  - `visible_grid_capture_targets_the_ephemeral_pane_id`

  The fake tmux log must show, in order:

  ```text
  display-message -p -t %15 <length-prefixed metadata format>
  capture-pane -p -t %15
  display-message -p -t %15 <same metadata format>
  ```

  Assert no `capture-pane -J`, capture-history start/end option, `set-buffer`, `paste-buffer`, or `send-keys` appears in a source-grid read. The existing root `-S <socket-path>` selector remains before the subcommand and is not confused with `capture-pane`'s separate `-S` history spelling. Metadata includes pane ID, width, height, cursor x/y, and `pane_in_mode`; accept only canonical ASCII `0` or `1` for mode.

- [ ] **Step 3: Run changed suites and confirm RED**

  ```bash
  cargo test --locked --test capture
  cargo test --locked --test tmux_source -- --nocapture --test-threads=1
  cargo test --locked --bin tmux-rescue
  ```

  Expected failures: the capability has no grid read, source records have no pane ID, and Codex recovery cannot be enriched.

- [ ] **Step 4: Implement stable adapter sampling**

  Add `pane_id: TmuxPaneId` to `TopologyPane`, its constructor/getter, and the topology fingerprint; `RawPaneSnapshot` remains unchanged. Add `#{pane_id}` to `SOURCE_FORMAT` and parse it directly into that refined value. Update `CreatedPane` and `RestoredPane` to retain the already-parsed `TmuxPaneId`, so restore-side process observation can construct `TopologyPane` without discarding and rechecking pane-ID proof. Synchronize affected process and target fixtures in the same compiling slice.

  Implement `TmuxAdapter::read_visible_pane` with two independently parsed metadata samples around `capture-pane -p`. Require equality of pane ID, width, height, cursor, and mode before constructing `VisiblePaneGrid`.

  Reuse `source_client_command(Some(&self.selector))` so selector and `TMUX` handling remain identical to topology reads. Convert every external diagnostic through existing terminal-safe bounded text and never include captured stdout in an error.

- [ ] **Step 5: Enrich capture after exact classification**

  Refactor the foreground branch into a small helper that first records any resolver downgrade, then matches only:

  ```rust
  PaneRecovery::Automatic(AutomaticRecovery::Codex {
      session_id,
      prompt_area: None,
  })
  ```

  Read and parse the grid only for that exact variant. Attach `Captured`; silently retain `None` for `Absent`; emit `CodexPromptCaptureSkipped` and retain `None` for read/parser failure. Keep normal topology before/after validation around the enriched candidate.

  Before final validation, serialize the full raw candidate once to measure it against `MAX_SNAPSHOT_BYTES`. If prompt fields make it too large, recursively remove every `RawCapturedCodexPromptArea`, append one `CodexPromptCaptureSkipped` event per affected coordinate with reason `snapshot size budget exceeded`, and validate the stripped candidate. If the stripped candidate is still too large, return the existing `InvalidCandidate(SnapshotTooLarge)` error. This fallback is in-memory only and does not introduce a persisted count or schema field.

  Render the warning in `src/cli.rs` as:

  ```text
  warning: capture attempt 1 pane work:0:0 Codex prompt capture skipped: pane metadata changed
  ```

  Escape the failure through the existing safe renderer and print no rows.

- [ ] **Step 6: Run focused and adjacent suites**

  ```bash
  cargo test --locked --test capture
  cargo test --locked --test tmux_source -- --nocapture --test-threads=1
  cargo test --locked --bin tmux-rescue
  cargo test --locked --test cli
  ```

- [ ] **Step 7: Commit capture enrichment**

  ```bash
  git add src/capture.rs src/tmux.rs src/cli.rs tests/capture.rs tests/process_linux.rs tests/tmux_source.rs tests/tmux_target.rs
  git commit -m "feat: capture visible Codex prompt input"
  ```

---

### Task 5: Report counts and bind prompt input to its Codex launch

**Files:**

- Modify: `src/inspect.rs`
- Modify: `src/restore.rs`
- Modify: `tests/cli.rs`
- Modify: `tests/restore_plan.rs`

**Interfaces:**

```rust
pub struct PlannedAutomaticLaunch {
    directory: ExistingRecordedDirectory,
    input: LaunchableShellInput,
    expected: AutomaticRecoveryExpectation,
    codex_prompt: Option<PlannedCodexPromptPaste>,
}

pub struct PlannedCodexPromptPaste {
    input: CapturedCodexPromptArea,
}

pub enum PlannedPaneAction {
    LaunchAutomatic(PlannedAutomaticLaunch),
    // existing variants unchanged
}
```

- [ ] **Step 1: Add failing inspect privacy tests**

  Extend `PaneFact::ToolSession` with optional pending-input counts and add `explicit_inspect_reports_pending_input_counts_without_echoing_text` in `tests/cli.rs`. The exact detail, after session ID and before cwd, is:

  ```text
  pending input  5 visible rows · 49 bytes
  ```

  Update the existing mapped-recovery, topology, inspect, plain-tree, and forced-color fixtures. Assert the literal prompt is absent from stdout and stderr, and ANSI stripping preserves the plain bytes.

- [ ] **Step 2: Add failing opaque-plan tests**

  In `tests/restore_plan.rs`, add:

  - `codex_prompt_input_is_bound_to_its_enclosing_session_launch`
  - `claude_and_serve_launches_cannot_acquire_codex_prompt_input`
  - `automatic_fallback_drops_post_recovery_prompt_input`

  Extend `human_plan_prints_execution_relevant_fallbacks_and_inputs` to require:

  ```text
      after recovery  paste 5 visible rows without Enter
  ```

  Assert plan output contains counts but not the 49-byte prompt text. Treat opacity as an API-structure check: fields and constructors remain private to `restore.rs`, while integration tests receive values only from `plan_restore` and inspect them through read-only getters. Do not add an intentionally uncompilable ordinary integration test.

- [ ] **Step 3: Run and confirm RED**

  ```bash
  cargo test --locked --bin tmux-rescue inspect::tests
  cargo test --locked --test cli explicit_inspect_reports_pending_input_counts_without_echoing_text
  cargo test --locked --test restore_plan
  ```

  Expected failures: inspect has no pending-input fact and automatic launch has no prompt binding.

- [ ] **Step 4: Implement count-only inspection**

  Populate pending counts only from `AutomaticRecovery::Codex { prompt_area: Some(..), .. }`. Store counts, not text, in the private display view. Use singular `visible row` only for exactly one row; otherwise use `visible rows`. Leave aggregate program counts unchanged.

- [ ] **Step 5: Replace public parallel launch fields with one opaque value**

  Give `PlannedAutomaticLaunch` a private constructor that receives `&AutomaticRecovery` after executable/directory preflight. It constructs `expected` and retains prompt input in the same match:

  ```rust
  match automatic {
      AutomaticRecovery::Codex { prompt_area, .. } => prompt_area.clone(),
      AutomaticRecovery::ClaudeCode { .. }
      | AutomaticRecovery::MdBookServe { .. }
      | AutomaticRecovery::BookshelfServe { .. } => None,
  }
  ```

  Expose getters for directory, launch input, expectation, and a crate-visible method returning `Option<(&CodexSessionId, &CapturedCodexPromptArea)>`. That method matches the private `expected` and prompt fields together; callers cannot supply an unrelated session ID.

  Construct this value only after automatic preflight succeeds. The existing fallback returns before construction, structurally dropping prompt input. Update plan rendering and all pattern matches to use getters.

- [ ] **Step 6: Run inspect, plan, and CLI suites**

  ```bash
  cargo test --locked --bin tmux-rescue inspect::tests
  cargo test --locked --test restore_plan
  cargo test --locked --test cli
  cargo test --locked --lib
  ```

- [ ] **Step 7: Commit privacy-safe presentation and planning**

  ```bash
  git add src/inspect.rs src/restore.rs tests/cli.rs tests/restore_plan.rs
  git commit -m "feat: plan Codex prompt preparation"
  ```

---

### Task 6: Execute prompt preparation as a distinct partial outcome

**Files:**

- Modify: `src/restore.rs`
- Modify: `src/cli.rs`
- Modify: `tests/restore_execute.rs`
- Modify: `tests/cli.rs`

**Interfaces:**

```rust
pub type CodexPromptPasteResult = Result<(), CodexPromptPasteFailure>;

pub enum CodexPromptPasteFailure {
    SessionMismatch,
    PaneMissing,
    Failed(String),
}

pub trait RecoveryRestoreTarget {
    // existing methods
    fn paste_codex_prompt_area(
        &mut self,
        pane: &SourcePaneCoordinate,
        expected: &CodexSessionId,
        input: &CapturedCodexPromptArea,
    ) -> CodexPromptPasteResult;
}

pub enum PaneRestoreOutcome {
    // existing outcomes
    RecoveredAutomaticallyWithPromptPrepared,
    RecoveredAutomaticallyWithPromptNeedsAttention(CodexPromptPasteFailure),
}
```

- [ ] **Step 1: Add failing executor sequencing tests**

  Extend the fake recovery target with a scripted paste result and operation log. Add:

  - `recovered_codex_prepares_prompt_without_enter_after_fresh_identity_check`
  - `prompt_preparation_failure_is_partial_and_later_panes_continue`
  - `failed_or_fallback_automatic_recovery_never_pastes_prompt_input`
  - `automatic_recovery_without_prompt_retains_its_existing_outcome`

  Assert successful order is `launch -> settle observation -> paste_codex_prompt_area`. A mismatch/missing/failure creates the distinct recovered-but-needs-attention outcome, marks the overall run partial, performs no retry, and does not block later panes.

- [ ] **Step 2: Run executor tests and confirm RED**

  ```bash
  cargo test --locked --test restore_execute
  ```

  Expected failure: the target seam, result type, and pane outcomes do not exist.

- [ ] **Step 3: Implement executor branching**

  After `AutomaticPaneObservation::Recovered`, ask the opaque launch for a matching Codex prompt pair. Return the existing `RecoveredAutomatically` when absent. When present, call the new target operation exactly once and map success/failure to the two new outcomes. All shell-foreground fallback and failed observation branches return before prompt preparation.

  Include `RecoveredAutomaticallyWithPromptNeedsAttention` in `pane_outcome_is_partial`; successful preparation is complete.

- [ ] **Step 4: Add count-free outcome labels**

  Render exact stdout labels:

  ```text
  pane work:4:0: recovered automatically; prepared pending input
  pane work:4:0: recovered automatically; pending input needs attention (Codex session changed)
  ```

  Map `PaneMissing` and bounded `Failed` reasons similarly. Emit the existing partial warning on stderr. Labels contain neither prompt text nor serialized JSON.

- [ ] **Step 5: Run executor and CLI suites**

  ```bash
  cargo test --locked --test restore_execute
  cargo test --locked --test cli
  cargo test --locked --lib
  ```

- [ ] **Step 6: Commit executor outcomes**

  ```bash
  git add src/restore.rs src/cli.rs tests/restore_execute.rs tests/cli.rs
  git commit -m "feat: execute Codex prompt preparation"
  ```

---

### Task 7: Freshly verify the Codex session and paste literally in tmux

**Files:**

- Modify: `src/tmux.rs`
- Modify: `tests/tmux_target.rs`

- [ ] **Step 1: Add failing command-construction and identity tests**

  Add/extend tests:

  - unit: `codex_prompt_paste_is_set_buffer_then_bracketed_paste_without_enter`
  - integration: `changed_codex_identity_dispatches_no_tmux_input`
  - integration: `endpoint_replacement_before_prompt_paste_dispatches_no_tmux_input`
  - integration: `fresh_codex_identity_is_checked_after_settle_observation`

  The successful command vector must be exactly the existing literal helper's two commands over the original multiline UTF-8 bytes. Assert no third command and no argument `Enter`.

  Script process observations so settle returns Codex session A, the fresh paste operation returns either A or B, and only A-to-A dispatches input. The A-to-B case proves the settle result is not reused. Endpoint replacement must fail the owner condition before dispatch.

- [ ] **Step 2: Run tmux target tests and confirm RED**

  ```bash
  cargo test --locked --test tmux_target -- --nocapture --test-threads=1
  cargo test --locked --lib tmux::tests
  ```

  Expected failure: `TmuxOwnedTarget` does not implement the fresh prompt operation and its only pane guard requires a shell foreground command.

- [ ] **Step 3: Split shell and running-pane guards**

  Refactor the current guard without weakening it:

  ```rust
  fn running_pane_condition(&self, pane: &RestoredPane) -> String
  fn shell_pane_condition(&self, pane: &RestoredPane) -> String
  ```

  `running_pane_condition` combines server owner token/PID/start-time identity with `pane_dead == 0` and the recorded restored `pane_pid`. `shell_pane_condition` adds the existing `pane_current_command == target shell basename` clause. All existing verify/manual/fallback/launch operations continue to use the shell condition.

- [ ] **Step 4: Implement the deep prompt paste operation**

  In `paste_codex_prompt_area`:

  1. look up the restored pane or return `PaneMissing`;
  2. call `observe_pane` anew;
  3. classify foreground evidence and require `AutomaticRecovery::Codex` with the exact expected ID, ignoring any capture-only `prompt_area`;
  4. construct a unique owner-token-scoped buffer name;
  5. call `run_conditional_commands` with `running_pane_condition` and `literal_paste_commands` only;
  6. return `SessionMismatch` only when the fresh process classification differs; if the tmux conditional is blocked after an exact observation, probe for `PaneMissing` and otherwise return a terminal-safe `Failed("restore target or pane identity changed before prompt paste")` rather than misreporting it as a session mismatch.

  Never call `observe_automatic`, never sleep/retry, and never add `send-keys` in this operation.

- [ ] **Step 5: Run isolated tmux and restore suites**

  ```bash
  cargo test --locked --lib tmux::tests
  cargo test --locked --test tmux_target -- --nocapture --test-threads=1
  cargo test --locked --test restore_execute
  cargo test --locked --test e2e -- --nocapture --test-threads=1
  ```

- [ ] **Step 6: Commit guarded tmux preparation**

  ```bash
  git add src/tmux.rs tests/tmux_target.rs
  git commit -m "feat: restore Codex prompt input safely"
  ```

---

### Task 8: Synchronize docs, fixtures, and end-to-end evidence

**Files:**

- Modify: remaining matches reported by `rg -n 'AutomaticRecovery::Codex|"kind": "codex"' src tests`
- Modify: `README.md`
- Modify: `docs/src/DESIGN.md`
- Modify: `docs/src/ARCHITECTURE.md`
- Modify: `docs/src/TOOL-RECOVERIES.md`
- Modify: `tests/e2e.rs` only if an existing fixture needs the optional field or count-only output

- [ ] **Step 1: Audit every Codex fixture and privacy surface**

  Run:

  ```bash
  rg -n 'AutomaticRecovery::Codex|RawAutomaticRecovery::Codex|"kind": "codex"|pending input|prompt_area' src tests README.md docs/src
  ```

  Every constructor must deliberately set `prompt_area: None` or a validated fixture. Every match must explicitly consume it or use `..` only when prompt state is irrelevant. Search inspect/plan/warning/result output tests for the literal 49-byte prompt and require its absence.

- [ ] **Step 2: Add isolated mechanics evidence**

  In `tests/tmux_source.rs`, add a real temporary-server test that writes the approved seven-row bottom suffix into a uniquely named pane, runs the stable visible-grid read, and proves row boundaries/blank rows are retained without `-J`. In `tests/tmux_target.rs`, keep the real temporary-server literal paste assertion and extend it to multiline text with no submitted command.

  These tests must use unique `-L` or `-S` selectors under temporary directories, serialize with `--test-threads=1`, and never contact or mutate the default tmux server. Do not add a host-dependent real Codex e2e.

- [ ] **Step 3: Update user and architecture documentation top-down**

  Apply only these role-specific changes:

  - `README.md`: narrow the current unsaved-terminal-input exclusion at line 128 to hidden, scrolled-out, or unsupported input; document explicit-snapshot capture, count-only inspect/plan output, no-Enter restore, and plaintext sensitivity.
  - `docs/src/DESIGN.md`: update Snapshot, Inspect, What v1 Preserves, Safety Model, and Non-Goals with the user-visible best-effort contract.
  - `docs/src/ARCHITECTURE.md`: own raw/validated types, grid/refinement boundary, pane-ID fingerprint, private parser, optional-enrichment aggregate fallback, capture failures, opaque plan binding, fresh target operation, outcomes, privacy, schema compatibility, and non-atomic observation limitation.
  - `docs/src/TOOL-RECOVERIES.md`: update the Codex variant shape and document the exact 0.145.0 renderer fixture, both glyphs, literal paste placeholders, visible-only limits, and exact-session post-recovery rule.

  Do not add an mdBook page or change `docs/src/SUMMARY.md`.

- [ ] **Step 4: Run focused regression gates**

  ```bash
  cargo test --locked --test model
  cargo test --locked --test capture
  cargo test --locked --test restore_plan
  cargo test --locked --test restore_execute
  cargo test --locked --bin tmux-rescue inspect::tests
  cargo test --locked --test cli
  cargo test --locked --test tmux_source -- --nocapture --test-threads=1
  cargo test --locked --test tmux_target -- --nocapture --test-threads=1
  cargo test --locked --test e2e -- --nocapture --test-threads=1
  ```

- [ ] **Step 5: Run the full release-equivalent verification**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features --locked -- -D warnings
  cargo test --all-targets --all-features --locked -- --test-threads=1
  cargo build --release --locked
  cargo doc --all-features --locked --no-deps
  cargo test --doc --all-features --locked
  mdbook build docs
  ```

  Record exact test counts and any environment-dependent skip honestly.

- [ ] **Step 6: Scan for unfinished plan markers and inspect the final diff**

  ```bash
  rg -n 'TODO|TBD|FIXME|XXX|placeholder|unimplemented!|todo!' Cargo.toml src tests README.md docs/src
  git diff --check
  git status --short
  git diff --stat HEAD~7..HEAD
  ```

  Existing product phrases such as Codex's literal `[Pasted Content N chars]` and documented empty-composer placeholder are allowed; no implementation placeholder or deferred branch may remain.

- [ ] **Step 7: Commit docs and final integration evidence**

  ```bash
  git add README.md docs/src/DESIGN.md docs/src/ARCHITECTURE.md docs/src/TOOL-RECOVERIES.md tests/tmux_source.rs tests/tmux_target.rs tests/e2e.rs
  git commit -m "docs: document Codex prompt recovery"
  ```

- [ ] **Step 8: Verify the clean package and final repository state**

  ```bash
  cargo package --list --locked
  cargo package --locked
  git status --short
  ```

  Confirm the package list includes the new Rust modules and updated docs required by the existing manifest rules, both package commands exit successfully from the committed clean tree, and `git status --short` is empty.

---

## Completion Checklist

- [ ] A snapshot of the approved multiline fixture stores exactly `The test prompt for recovering.\n\nLine 1.\n\nLine 2.` under its exact Codex recovery.
- [ ] Existing Codex snapshots load with no prompt input and serialize without a `prompt_area` field.
- [ ] Unsupported or changing screens retain automatic Codex session recovery and report no prompt text.
- [ ] Inspect and plan output show `5 visible rows` and `49 bytes` but never the draft.
- [ ] A matching restored Codex session receives the exact multiline bytes through bracketed paste and no Enter.
- [ ] Mismatch, fallback, missing pane, or ownership loss dispatches no prompt input and produces a partial/attention result where appropriate.
- [ ] Non-Codex capture and restore behavior remains covered by existing suites.
- [ ] Full Rust, tmux-isolated, documentation, build, and package gates pass.
