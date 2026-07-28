# Codex Empty Composer Style Proof Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop false unsupported-layout warnings for normal Codex `0.145.0`
empty composers while preserving real drafts whose text collides with a Codex
suggestion.

**Architecture:** Keep `VisiblePaneGrid` as a deep module: one styled-byte
constructor consumes tmux SGR and exposes plain rows plus one crate-private,
atomic faint-suffix proof. Keep renderer policy in `codex_prompt`, command
execution in the tmux adapter, and document the two local extension points;
add no generic terminal model, parser dependency, renderer trait, or registry.

**Tech Stack:** Rust 2024, tmux 3.4 `capture-pane -e`, existing
`unicode-width` 0.2, mdBook 0.5.3

## Global Constraints

- The behavior authorities are
  `docs/superpowers/specs/2026-07-25-codex-visible-prompt-recovery-design.md`
  and
  `docs/superpowers/specs/2026-07-28-codex-empty-composer-style-proof-design.md`;
  the latter supersedes only the empty-composer and footer-position clauses.
- Capture exactly one full visible grid with
  `tmux capture-pane -p -e -t <exact-pane-id>` between the existing identical
  metadata reads. Add no second content read, `-J`, history `-S`/`-E`, tmux
  buffer command, or input command.
- Decode raw bytes before UTF-8/grid refinement. Admit only seven-bit `ESC [`
  CSI with final `m`, no intermediate/private bytes, decimal semicolon
  parameters, no internal empty parameter, and the finite SGR vocabulary from
  the follow-up design. Admit extended `38`, `48`, and `58` only as `5;n` or
  `2;r;g;b` with every payload in `0..=255`; reject colon forms and every
  unknown operation.
- Require every ordinary byte run between escape sequences to be independently
  valid UTF-8. An escape may occur between Unicode scalars, never inside one.
- Apply SGR left to right. Empty/`0` clears faint, `2` sets faint, and `22`
  clears faint. Other admitted operations leave faint unchanged. Extended
  color payload values never act as standalone attributes. Style state spans
  newline-delimited rows.
- Raw ANSI, mutable decoder state, and detached style booleans never leave
  `visible_pane`. `VisibleRow` may return only an opaque proof borrowing the
  exact nonempty suffix after proving every prefix character non-faint and
  every suffix character faint.
- Only these exact Codex `0.145.0` suggestions can mean `Absent`, and only
  through that proof: `Ask Codex to do anything`, `Implement {feature}`,
  `Use /skills to list available skills`, `Write tests for @filename`,
  `Explain this codebase`, and `Find and fix a bug in @filename`.
- A supported bottom layout is exactly one-or-more blank inset rows, one footer
  accepted by the existing footer recognizer, then zero-or-more blank terminal
  rows. Do not widen the footer recognizer.
- Plain, partially faint, reset-inside, all-row-faint, or unknown faint text at
  cursor cell two remains `Skipped`. Real draft capture remains style-agnostic
  and must preserve the existing five-row, 49-byte fixture exactly.
- Decoder and layout failures are bounded and prompt-free. They preserve exact
  automatic Codex session recovery and emit the existing skip warning. The
  external-editor/no-open-session downgrade remains unchanged.
- Keep future changes local: SGR vocabulary and representation in
  `visible_pane`; versioned suggestion/footer/layout policy in `codex_prompt`;
  command flags in `tmux`. Do not add a terminal emulator, public generic style
  map, renderer abstraction, configuration file, or runtime allowlist.
- Add no dependency and change no snapshot schema, restore behavior,
  inspection/plan rendering, external-editor fallback, or non-Codex recovery.
- The user already authorized draft PR #2 and this follow-up patch. Final
  handoff may push only `feat/codex-visible-prompt-recovery` to refresh that
  draft PR; do not merge it, publish a crate, or create another PR.
- Every production slice follows strict RED -> GREEN -> REFACTOR: add a focused
  behavioral test, run it and record the expected failure, implement the
  minimum behavior, rerun the focused test, then run the adjacent suite.
- Keep commits surgical and task-local; do not refactor adjacent capture,
  recovery, restore, storage, CLI, or documentation code.

## File Map

- `src/visible_pane.rs`: own the strict styled-byte decoder, private effective
  faintness representation, existing grid invariants, and atomic proof
  interface. This is the only module that knows SGR syntax.
- `src/codex_prompt.rs`: own the versioned six-string policy, composer grammar,
  existing footer recognizer, and `Absent | Captured | Skipped` decision.
- `src/tmux.rs`: request one styled visible capture and preserve the existing
  metadata fence; know nothing about Codex suggestions.
- `tests/tmux_source.rs`: prove exact command ordering/flags and the real-tmux
  styled-output assumption through the existing isolated-server harness.
- `tests/capture.rs`: prove style-backed absence is silent and a plain-text
  collision remains a prompt-free skip without losing automatic recovery.
- `docs/src/ARCHITECTURE.md`: explain the styled-grid seam, opaque proof,
  prompt-free failure, and narrow maintenance path.
- `docs/src/TOOL-RECOVERIES.md`: state the six versioned suggestions, required
  style transition, and footer-plus-blank-tail grammar.
- No production file is created. `Cargo.toml`, `Cargo.lock`, model, restore,
  storage, CLI, inspect, and external-editor recovery remain untouched.

---

### Task 1: Refine Styled Tmux Output Into One Atomic Proof

**Files:**

- Modify: `src/visible_pane.rs`
- Modify: `src/tmux.rs` (constructor rename only)
- Modify: `src/codex_prompt.rs` (test-helper constructor rename only)
- Modify: `tests/capture.rs` (fixture constructor rename only)

**Interfaces:**

- Consumes: `VisiblePaneMetadata` and the raw bytes returned by tmux.
- Produces:

  ```rust
  pub struct VisibleRow {
      text: String,
      faint_by_char: Vec<bool>,
  }

  pub(crate) struct FaintVisibleText<'a>(&'a str);

  impl FaintVisibleText<'_> {
      pub(crate) fn as_str(&self) -> &str;
  }

  impl VisibleRow {
      pub fn as_str(&self) -> &str;

      pub(crate) fn faint_suffix_after_non_faint_prefix(
          &self,
          prefix: &str,
      ) -> Option<FaintVisibleText<'_>>;
  }

  impl VisiblePaneGrid {
      pub fn try_from_tmux_styled_capture(
          metadata: VisiblePaneMetadata,
          output: Vec<u8>,
      ) -> Result<Self, VisiblePaneGridError>;
  }
  ```

- Invariants: `faint_by_char.len() == text.chars().count()`; the proof is
  privately constructed, non-detachable, and borrows the exact suffix; the old
  `try_from_tmux_capture` constructor is removed rather than retained as a
  bypass.

- [ ] **Step 1: Write failing styled-grid and proof tests**

  In the existing `src/visible_pane.rs` test module, add these behavioral tests:

  ```rust
  #[test]
  fn strips_sgr_and_proves_only_a_faint_suffix_after_a_non_faint_prefix()

  #[test]
  fn rejects_incomplete_prefix_or_suffix_style_evidence()

  #[test]
  fn applies_intensity_operations_left_to_right()

  #[test]
  fn carries_faint_state_across_rows()

  #[test]
  fn accepts_the_finite_sgr_vocabulary_without_treating_color_payload_as_style()

  #[test]
  fn rejects_unsupported_or_malformed_terminal_sequences_without_echoing_text()

  #[test]
  fn preserves_plain_grid_controls_width_and_final_empty_row_rules_after_decoding()
  ```

  Use the literal positive fixture
  `"\x1b[1m› \x1b[22;2mImplement {feature}\x1b[0m\n".as_bytes()`. Assert the
  plain row is exactly `› Implement {feature}` and the proof for `› ` exposes
  exactly `Implement {feature}`.

  The evidence-negative table must include:

  ```rust
  [
      "› Implement {feature}\n".as_bytes(),
      "\x1b[2m› Implement {feature}\x1b[0m\n".as_bytes(),
      "›\x1b[2m Implement {feature}\x1b[0m\n".as_bytes(),
      "› \x1b[2mImplement\x1b[22m {feature}\n".as_bytes(),
      "› \n".as_bytes(),
  ]
  ```

  Prove ordering with `2;22` (no proof) and `22;2` (proof). Prove state spans
  rows with a first row that enters faint and a second row whose leading text
  must remain faint until `22`; a decoder that resets at newline must make the
  assertion fail.

  Admit representative simple-range boundaries plus both `38;5;n` and
  `38;2;r;g;b` (also `48` and `58`). Put `2` and `22` inside color payloads and
  assert they do not change intensity. Reject at least:

  ```text
  bare ESC; ESC followed by a non-[ byte; ESC[2K; ESC[?2m; ESC[2:m;
  ESC[2;m; ESC[38m; ESC[38;5m; ESC[38;5;256m;
  ESC[38;2;0;0m; ESC[38;2;0;0;256m; ESC[56m; ESC[66m;
  ESC[76m; ESC[108m; C1 CSI; malformed UTF-8; ESC inside a UTF-8 scalar
  ```

  Include sensitive adjacent text in one malformed case and assert neither
  `Display` nor `Debug` for the error contains that text or raw escape bytes.
  Retain the existing Unicode-width, control-character, row-count, missing
  delimiter, and final-empty-row tests through the renamed constructor.

- [ ] **Step 2: Run the styled-grid tests and confirm RED**

  ```bash
  cargo test --locked --lib visible_pane::tests
  ```

  Expected: compilation fails because
  `try_from_tmux_styled_capture`, `FaintVisibleText`, and
  `faint_suffix_after_non_faint_prefix` do not exist. If a test instead passes
  through the old plain constructor, remove that bypass before proceeding.

- [ ] **Step 3: Implement the private decoder and refined row**

  Replace the old constructor and keep the implementation inside the existing
  `visible_pane` module. Use fixed, prompt-free error variants such as:

  ```rust
  #[error("tmux styled capture contains a truncated escape sequence")]
  TruncatedEscapeSequence,
  #[error("tmux styled capture contains a non-SGR escape sequence")]
  NonSgrEscapeSequence,
  #[error("tmux styled capture contains malformed SGR parameters")]
  MalformedSgrParameters,
  #[error("tmux styled capture contains an unsupported SGR operation")]
  UnsupportedSgrOperation,
  ```

  Decode bytes with a private `faint: bool` state. On `ESC`, first decode the
  preceding maximal ordinary byte run with `std::str::from_utf8`, append its
  Unicode scalars, and assign the current faint state to each scalar. Then
  require the next byte to be `[`, consume only ASCII digits/semicolons until
  `m`, parse the parameter operations in order, and consume extended-color
  groups atomically. Flush the final ordinary run the same way. This makes an
  escape inserted inside a multibyte scalar fail UTF-8 refinement instead of
  combining bytes with conflicting style. Remove exactly one final newline and
  its style entry, split rows without trimming, and run the existing
  control/height/width checks.

  Implement the proof from the refined representation:

  ```rust
  let suffix = self.text.strip_prefix(prefix)?;
  let prefix_chars = prefix.chars().count();
  if prefix_chars == 0
      || suffix.is_empty()
      || !self.faint_by_char[..prefix_chars].iter().all(|faint| !faint)
      || !self.faint_by_char[prefix_chars..].iter().all(|faint| *faint)
  {
      return None;
  }
  Some(FaintVisibleText(suffix))
  ```

  Rename every live call site to `try_from_tmux_styled_capture`; do not change
  tmux flags or Codex policy in this task.

- [ ] **Step 4: Run focused and adjacent suites**

  ```bash
  cargo test --locked --lib visible_pane::tests
  cargo test --locked --lib codex_prompt::tests
  cargo test --locked --test capture
  cargo test --locked --lib
  ```

  Expected: all pass. The existing plain fixtures remain valid styled captures
  with no escape sequences.

- [ ] **Step 5: Commit the styled-grid refinement**

  ```bash
  git add src/visible_pane.rs src/tmux.rs src/codex_prompt.rs tests/capture.rs
  git commit -m "feat: refine styled tmux pane grids"
  ```

---

### Task 2: Request One Styled Capture Through the Existing Metadata Fence

**Files:**

- Modify: `src/tmux.rs`
- Modify: `tests/tmux_source.rs`

**Interfaces:**

- Consumes: `VisiblePaneGrid::try_from_tmux_styled_capture` from Task 1.
- Produces: the unchanged `CaptureSource::read_visible_pane` interface, backed
  by exactly `display metadata -> capture-pane -p -e -> display metadata`.

- [ ] **Step 1: Make adapter and real-tmux assumptions fail first**

  Update `install_visible_grid_fake_tmux` so its capture case matches exactly:

  ```text
  capture-pane -p -e -t %15
  ```

  Make the stable fake emit one accepted SGR sequence around visible text and
  keep the observable plain-row assertion unchanged. In
  `visible_grid_capture_uses_stable_metadata_and_never_joins_rows`, require:

  ```text
  ARG=capture-pane
  ARG=-p
  ARG=-e
  ARG=-t
  ARG=%15
  ```

  Keep the three-command count, selector-before-subcommand check, and forbidden
  `-J`, history-bound, buffer, paste, and `send-keys` assertions.

  Add `real_adapter_returns_the_faint_suffix_proof` inside the crate's existing
  `src/tmux.rs` test module so it may use the crate-private proof without
  widening the production interface. Use a unique `tempfile` `-S` socket, a
  `/dev/null` tmux config, and a short renderer script that writes non-faint
  `› ` followed by `\033[2mImplement {feature}\033[0m`. Construct the real
  `TmuxAdapter`, call its source-topology and visible-pane paths, and assert the
  returned escape-free row plus the exact proof. Use an RAII server guard so a
  panic still kills only that temporary server; do not call tmux directly for
  the assertion and do not touch the user's tmux server.

- [ ] **Step 2: Run the fake adapter test and confirm RED**

  ```bash
  cargo test --locked --test tmux_source \
    visible_grid_capture_uses_stable_metadata_and_never_joins_rows \
    -- --nocapture --test-threads=1
  ```

  Expected: the fake reports an unexpected command because the adapter omitted
  `-e`. The failure must be about the missing flag, not renderer timing.

- [ ] **Step 3: Add only the styled-output flag**

  Change the adapter command to:

  ```rust
  .args([
      "capture-pane",
      "-p",
      "-e",
      "-t",
      pane.pane_id().as_str(),
  ])
  ```

  Keep the existing metadata closure, before/after equality check, pane-ID
  equality check, one command execution, error mapping, and no-start selected
  client unchanged.

- [ ] **Step 4: Run focused and full adapter suites**

  ```bash
  cargo test --locked --test tmux_source \
    visible_grid_capture_uses_stable_metadata_and_never_joins_rows \
    -- --nocapture --test-threads=1
  cargo test --locked --lib real_adapter_returns_the_faint_suffix_proof \
    -- --nocapture --test-threads=1
  cargo test --locked --test tmux_source -- --nocapture --test-threads=1
  ```

  Expected: all pass; the real test reads only its unique temporary socket.

- [ ] **Step 5: Commit the adapter change**

  ```bash
  git add src/tmux.rs tests/tmux_source.rs
  git commit -m "fix: preserve style in visible pane capture"
  ```

---

### Task 3: Classify Only Style-Proven Codex Suggestions and Shifted Footers

**Files:**

- Modify: `src/codex_prompt.rs`
- Modify: `tests/capture.rs`

**Interfaces:**

- Consumes:
  `VisibleRow::faint_suffix_after_non_faint_prefix(&str) -> Option<FaintVisibleText<'_>>`.
- Produces: the existing `CodexPromptAreaObservation`; only the conditions for
  `Absent` and accepted footer position change.

- [ ] **Step 1: Add failing parser and capture behavior tests**

  Replace the plain empty-composer fixture with a helper that renders:

  ```text
  › <SGR 2><suggestion><SGR 22>
  <one or more blank inset rows>
  <recognized footer>
  <zero or more blank terminal rows>
  ```

  Add these parser tests in `src/codex_prompt.rs`:

  ```rust
  returns_absent_for_each_style_proven_codex_0145_suggestion
  skips_known_suggestions_without_complete_style_proof
  skips_an_unknown_faint_suggestion
  keeps_the_five_row_real_draft_exact_and_style_agnostic
  accepts_a_recognized_footer_followed_by_blank_terminal_rows
  rejects_nonblank_or_duplicate_rows_around_the_footer
  ```

  The first test must use a literal table containing all six strings from the
  Global Constraints and both accepted prompt glyphs across the cases. The
  collision table must cover the same known text unstyled, the entire row
  faint, only part of the suffix faint, and `22` inside the suffix. Unknown
  fully faint text remains `Skipped`.

  Reuse the existing 49-byte assertion verbatim. Test footer-last and
  footer-plus-five-blank-rows for both a real draft and a style-proven empty
  composer. Negative footer fixtures must include: no blank inset, missing
  footer, nonempty text before the footer, a second recognized footer, and
  nonempty text after the footer.

  In `tests/capture.rs`, make `absent_grid` style-proven. Add
  `unstyled_suggestion_retains_automatic_recovery_and_emits_one_safe_warning`:
  assert `AutomaticRecovery::Codex { prompt_area: None, .. }`, exactly one
  `CodexPromptCaptureSkipped` event, and no suggestion text in `Display` or
  `Debug` diagnostics.

- [ ] **Step 2: Run parser and capture tests and confirm RED**

  ```bash
  cargo test --locked --lib codex_prompt::tests
  cargo test --locked --test capture
  ```

  Expected: new rotating suggestions remain `Skipped`, an unstyled copy of the
  old singleton is incorrectly `Absent`, and footer-plus-blank-tail fixtures
  are rejected. These are the three missing production branches.

- [ ] **Step 3: Implement the versioned policy and footer-layout parser**

  Replace the singleton with one local, documented policy table:

  ```rust
  const SUPPORTED_CODEX_0145_EMPTY_SUGGESTIONS: [&str; 6] = [
      "Ask Codex to do anything",
      "Implement {feature}",
      "Use /skills to list available skills",
      "Write tests for @filename",
      "Explain this codebase",
      "Find and fix a bug in @filename",
  ];
  ```

  At cursor cell two on the first/only prompt row, obtain the proof from the
  complete `VisibleRow`, then compare only `proof.as_str()` with that table.
  Never compare the unproved string to choose `Absent`.

  Replace the footer-last condition with one private parser that returns a
  named proof rather than validating and continuing with an unrefined value:

  ```rust
  struct SupportedFooterLayout;

  fn parse_supported_footer_layout(
      rows: &[VisibleRow],
      cursor_y: usize,
  ) -> Option<SupportedFooterLayout> {
      let after_cursor = rows.get(cursor_y + 1..)?;
      let footer_offset = after_cursor
          .iter()
          .position(|row| !row.as_str().is_empty())?;
      if footer_offset == 0 {
          return None;
      }
      let footer_y = cursor_y + 1 + footer_offset;
      if !is_supported_codex_0145_footer(rows[footer_y].as_str())
          || rows[footer_y + 1..]
              .iter()
              .any(|row| !row.as_str().is_empty())
      {
          return None;
      }
      Some(SupportedFooterLayout)
  }
  ```

  Bind the result before continuing; leave the existing footer recognizer and
  real-draft row extraction unchanged. Do not widen the prompt rows past the
  cursor.

- [ ] **Step 4: Run focused and adjacent suites**

  ```bash
  cargo test --locked --lib codex_prompt::tests
  cargo test --locked --test capture
  cargo test --locked --lib visible_pane::tests
  cargo test --locked --lib
  ```

  Expected: all pass, including the exact 49-byte draft and prompt-free warning
  assertions.

- [ ] **Step 5: Commit the Codex policy correction**

  ```bash
  git add src/codex_prompt.rs tests/capture.rs
  git commit -m "fix: recognize style-proven Codex suggestions"
  ```

---

### Task 4: Document the Stable Seam and Narrow Extension Path

**Files:**

- Modify: `docs/src/ARCHITECTURE.md`
- Modify: `docs/src/TOOL-RECOVERIES.md`

**Interfaces:**

- Consumes: the implemented constructor, opaque proof, versioned suggestion
  table, footer grammar, and exact tmux command from Tasks 1-3.
- Produces: maintainer and user documentation only; no runtime interface.

- [ ] **Step 1: Update the architecture role from the module outward**

  In `Optional Codex Prompt Enrichment`, change the capture sequence to
  `capture-pane -p -e -t <exact-pane-id>`. Explain that `visible_pane` consumes
  all admitted SGR, retains only private per-character faintness, and exposes
  plain rows plus one atomic proof; raw ANSI and generic style queries cannot
  cross the seam. State that malformed/unsupported SGR fails optional prompt
  enrichment with a fixed prompt-free diagnostic while exact session recovery
  remains available.

  Add a short `Maintaining renderer evidence` subsection with exactly these
  routes:

  1. A newly observed tmux SGR form changes only the private decoder and its
     RED boundary fixture.
  2. A newly observed Codex suggestion changes the versioned table, a
     style-backed collision fixture, and `TOOL-RECOVERIES.md`.
  3. A new Codex layout or version gets explicit fixtures and versioned policy;
     it must not weaken the current faint proof or footer recognizer.

  Explicitly say no renderer registry or general terminal emulation is part of
  this seam.

- [ ] **Step 2: Update the user-facing Codex renderer contract**

  In `TOOL-RECOVERIES.md`, replace the singleton empty-composer rule with the
  six exact suggestions. State that every character of `› ` or `» ` must be
  effectively non-faint, every suggestion character must be faint, the cursor
  must be at cell two, and plain/partial/unknown text is an unsupported layout
  rather than proven empty.

  Change the bottom grammar to one-or-more blank inset rows, one existing
  recognized footer, and zero-or-more blank terminal rows. Preserve the current
  visible-suffix, privacy, literal `[Pasted Content ...]`, and external-editor
  wording.

- [ ] **Step 3: Build the documentation and inspect the focused diff**

  ```bash
  mdbook build docs
  git diff --check
  git diff -- docs/src/ARCHITECTURE.md docs/src/TOOL-RECOVERIES.md
  ```

  Expected: mdBook succeeds; the diff changes only the styled-capture,
  empty-composer, footer-position, and maintenance clauses.

- [ ] **Step 4: Commit the documentation**

  ```bash
  git add docs/src/ARCHITECTURE.md docs/src/TOOL-RECOVERIES.md
  git commit -m "docs: explain Codex style-proof extension seam"
  ```

---

## Final Controller Verification

After all task reviews and the whole-branch review are clean, run the exact
release-equivalent gates from the repository workflow and prior PR handoff:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked -- --test-threads=1
cargo build --release --locked
cargo doc --all-features --locked --no-deps
cargo test --doc --all-features --locked
mdbook build docs
cargo package --list --locked
cargo package --locked
```

Then confirm the worktree is clean, push
`feat/codex-visible-prompt-recovery`, refresh draft PR #2, and verify the GitHub
Rust check. Do not publish a crate, merge the PR, or alter the legitimate
external-editor downgrade.
