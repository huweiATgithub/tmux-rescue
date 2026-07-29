# Codex Status Footer Trust Composition Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recognize configured Codex status footers through version-neutral
visible invariants while preserving prompt bytes exactly and failing closed
when footer evidence is insufficient.

**Architecture:** Keep all renderer recognition private to
`src/codex_prompt.rs`: first refine footer placement, then parse exact
instructional output or configured textual evidence, and expose only a
proof-bearing supported-footer result to prompt extraction. Keep capture
orchestration, session identity, snapshot data, and style decoding unchanged;
update only the fixed unsupported-layout message and the normative
documentation around this seam.

**Tech Stack:** Rust 2024, `unicode-width` 0.2, existing styled
`VisiblePaneGrid`, Cargo 1.94, mdBook 0.5.3

## Global Constraints

- The behavior authorities are
  `docs/superpowers/specs/2026-07-25-codex-visible-prompt-recovery-design.md`,
  `docs/superpowers/specs/2026-07-28-codex-empty-composer-style-proof-design.md`,
  and
  `docs/superpowers/specs/2026-07-29-codex-status-footer-trust-composition-design.md`.
  The July 29 design supersedes only configured-footer recognition and
  configured-footer truncation from the July 28 record.
- Codex `0.145.0` is verified fixture provenance, never a runtime gate.
  Do not read `codex --version`, accept a version argument, dispatch on a
  release number, or put `0145` in a production type or function name.
- Keep the placement envelope mandatory: exact Codex session classification,
  no tmux copy mode, supported prompt/cursor grammar, one-or-more blank inset
  rows, exactly one remaining nonempty footer row, and only blank terminal rows.
- A footer candidate begins with exactly two ASCII spaces and no third leading
  space. Indentation, separators, placement, style, and ellipsis are syntax,
  never trust votes.
- Accept a configured footer only for one complete high-trust signal or two
  distinct weak families. High signals are bounded
  `Context N% used|left` and a complete leading `gpt-` model selection.
  Weak families are Model, Workspace, Accounting, Runtime, Git, and Identity.
- Count each weak family at most once and each segment as at most one family.
  Do not introduce numeric scoring, caller-visible counts, or independently
  recombinable predicate results.
- Parse model, context, compact-count, dotted-version, UUID, and right-indicator
  syntax exactly as specified. Malformed near-matches contribute no evidence.
- A terminal U+2026 `…` contributes no evidence. Ignore the entire incomplete
  final segment; never reconstruct its suffix. Reject ellipsis anywhere else.
- Strip a recognized right indicator only when its exact text and display-cell
  geometry match the renderer contract. It contributes no trust.
- Exact shortcut, queue, legacy bounded context, and Plan instructional footers
  remain a separate accepted production.
- Configured-footer recognition consumes decoded plain text only. Theme-like
  colors, all-faint rendering, and no SGR must make the same decision.
  Faintness remains private evidence solely for empty-composer classification.
- On drift or insufficient evidence, omit only optional `prompt_area` and use
  the fixed version-neutral warning
  `visible pane does not match a supported Codex prompt layout`. Preserve
  exact automatic Codex session recovery and diagnostic privacy.
- Preserve the live five-row draft as exactly 49 UTF-8 bytes. Do not change
  snapshot schema, restore behavior, session identity, prompt serialization,
  visible-grid refinement, external-editor behavior, or non-Codex recovery.
- Add no dependency or source module. Keep `Cargo.toml` and `Cargo.lock`
  unchanged and leave the July 28 historical design record untouched.
- The user already selected subagent-driven execution and authorized updating
  draft PR #2. Push only
  `feat/codex-visible-prompt-recovery` after all verification; do not merge,
  publish a crate, or create another PR.
- Every production change follows RED -> GREEN -> REFACTOR: add a focused
  behavioral assertion, run it and observe the intended failure, implement the
  minimum behavior, rerun the focused suite, then run adjacent checks.

## File Map

- `src/codex_prompt.rs`: own footer placement, right-indicator geometry,
  exact instructional parsing, configured evidence extraction/composition,
  terminal truncation, prompt extraction, and focused unit fixtures.
- `src/capture.rs`: own the existing typed failure and change only the
  unsupported-layout message text.
- `tests/capture.rs`: keep one configured-default integration happy path and
  assert the exact version-neutral warning without exposing prompt text.
- `docs/src/TOOL-RECOVERIES.md`: describe the operator-visible prompt
  evidence and best-effort cross-version behavior.
- `docs/src/ARCHITECTURE.md`: describe the private refined-type seam,
  evidence composition, style independence, and extension discipline.
- Create no production file. Do not modify `src/visible_pane.rs`,
  `Cargo.toml`, `Cargo.lock`, snapshot/restore modules, or either prior
  design record.

---

### Task 1: Replace the Versioned Boolean With a Proof-Bearing Footer Parser

**Files:**

- Modify: `src/codex_prompt.rs:11-245`
- Modify tests: `src/codex_prompt.rs:254-703`

**Interfaces:**

- Consumes: `&[VisibleRow]`, the cursor row index, and
  `grid.metadata().width().get()`.
- Produces these private proof-bearing values:

  ```rust
  struct PositionedFooterCandidate<'a> {
      content: &'a str,
      row_width: usize,
      pane_width: usize,
  }

  enum SupportedCodexFooter {
      Instructional(ExactInstructionalFooter),
      Configured(ConfiguredFooterBasis),
  }

  struct ExactInstructionalFooter;

  enum ConfiguredFooterBasis {
      High(HighTrustSignal),
      Corroborated(CorroboratedWeakSignals),
  }

  enum ConfiguredFooterEvidence {
      High(HighTrustSignal),
      Weak(WeakEvidence),
  }

  enum HighTrustSignal {
      Context,
      LeadingModel,
  }

  #[derive(Clone, Copy, Eq, PartialEq)]
  enum WeakSignalFamily {
      Model,
      Workspace,
      Accounting,
      Runtime,
      Git,
      Identity,
  }

  struct CorroboratedWeakSignals {
      _first: WeakSignalFamily,
      _second: WeakSignalFamily,
  }
  ```

- `capture_visible_codex_prompt` keeps its existing signature and consumes
  `Option<SupportedCodexFooter>`; the current versioned Boolean seam is
  removed.
- The evidence accumulator is private and has only
  `None -> One(family) -> Corroborated(proof)` transitions. Repeated families
  do not advance it.

- [ ] **Step 1: Replace the obsolete truncation test with the positive evidence matrix**

  In the existing test module, replace
  `accepts_only_observed_terminal_context_truncations` with
  `accepts_high_or_two_distinct_low_footer_evidence`. Use exact cases:

  ```rust
  [
      "  Context 0% used",
      "  Context 100% used",
      "  Context 0% left",
      "  Context 100% left",
      "  gpt-5.6-sol ultra",
      "  gpt-5.4 xhigh fast",
      "  gpt-5.6-sol ultra · ~/projects/tmux-rescue",
      "  ~/projects/tmux-rescue · gpt-5.6-sol ultra",
      "  Fast on · 258K window",
      "  Working · 2.55M used",
      "  PR #2 · 0.146.0",
      "  No changes · 1d6381bf-01c5-4c4a-b725-8e376e5ad295",
      "  opaque · Fast on · 258K window",
  ]
  ```

  For each footer, construct `compact_grid(&["› pending"], 9, footer)` and
  assert the extracted text is exactly `pending`. Keep the existing
  `CONFIGURED_FOOTER` five-row fixture and its 49-byte assertion unchanged.

- [ ] **Step 2: Add RED tests for insufficient, correlated, and malformed evidence**

  Add `rejects_insufficient_correlated_or_malformed_footer_evidence` with
  these exact rejection groups:

  ```rust
  [
      "  main · gpt-5.6-sol ultra",
      "  ~/projects/tmux-rescue",
      "  258K window",
      "  Fast on",
      "  PR #2",
      "  0.146.0",
      "  258K window · 2.55M used",
      "  Fast on · raw output",
      "  main · gpt-5.6-sol ultra · gpt-5.4 high",
      "  Context 101% used",
      "  Context ９% left",
      "  prose mentioning gpt-5.6-sol ultra",
      "  ~/project · gpt-alpha",
      "  ~/project · gpt-5.6-sol super",
      "  Fast on · 2.555M used",
      "  PR #2 · v0.146.0",
      "  PR #2 · 0.146",
      "  PR #2 · 0.146.0-beta",
      "  PR #2 · 0..146",
      "  PR #2 · ０.146.0",
      "  Context 78% used ·  · Fast on",
      "  Context 78% used | Fast on",
  ]
  ```

  Every case must yield `CodexPromptAreaObservation::Skipped(_)`. These
  fixtures prove one weak family is insufficient, correlated repeats count
  once, and malformed structured values never supply a second vote.

  Add `parses_structured_signal_boundaries` against the private parsers.
  Accept model forms `gpt-5.6-sol`, `gpt-5.6-sol ultra`, and
  `gpt-5.4 xhigh fast`; compact counts `0`, `258K`, `2.5M`, and `2.55T`;
  dotted versions `0.146.0` and `12.0.300`; and lowercase or uppercase
  hexadecimal canonical UUIDs. Reject model forms `gpt-`, `gpt-alpha`,
  `gpt-5.6-sol super`, and `gpt-5.6-sol  ultra`; compact counts `-1`, `.5M`,
  `2.M`, `2.555M`, and `2.5m`; and the malformed dotted versions from the
  footer table.

- [ ] **Step 3: Add RED tests for neutral truncation and style independence**

  Add `uses_only_complete_evidence_before_terminal_truncation`:

  ```rust
  let accepted = [
      "  Context 78% used · ~/very/long/path…",
      "  gpt-5.6-sol ultra · ~/very/long/path…",
      "  Fast on · 258K window · unknown…",
  ];
  let rejected = [
      "  Context 48% u…",
      "  Context 48…",
      "  C…",
      "  gpt-5.6…",
      "  Context 78% used…",
      "  Context 78% used · path… · Fast on",
      "  Context 78% used · path…tail",
  ];
  ```

  Assert exact capture for `accepted` and `Skipped` for `rejected`.

  Add `configured_footer_recognition_is_style_independent` using the same
  decoded text `  Fast on · 258K window` in these three raw forms:

  ```rust
  [
      "  Fast on · 258K window",
      "\x1b[2m  Fast on · 258K window\x1b[22m",
      "  \x1b[38;5;1mFast on\x1b[39m \x1b[2m·\x1b[22m \
       \x1b[38;5;2m258K window\x1b[39m",
  ]
  ```

  Each form must capture exactly `pending`. Use the existing styled-grid
  constructor through `compact_grid`; do not expose a new style query.

- [ ] **Step 4: Add RED placement tests for exact indent, indicators, cursor-up, and popup layouts**

  Add a test-only helper:

  ```rust
  fn footer_with_right_indicator(width: usize, left: &str, indicator: &str) -> String {
      let occupied = UnicodeWidthStr::width(
          format!("{TEXTAREA_MARGIN}{left}{indicator}").as_str(),
      );
      let gap = width
          .checked_sub(2 + occupied)
          .filter(|gap| *gap >= 1)
          .unwrap();
      format!("{TEXTAREA_MARGIN}{left}{}{indicator}", " ".repeat(gap))
  }
  ```

  Add `accepts_only_exact_right_aligned_indicator_geometry`. At pane width
  `80`, accept a context-only high signal with each indicator:

  ```rust
  [
      "Plan mode",
      "Plan mode (shift+tab to cycle)",
      "IDE context",
      "Plan mode · IDE context",
      "Plan mode (shift+tab to cycle) · IDE context",
  ]
  ```

  Reject `Review mode`, a recognized indicator ending one cell too early,
  and one ending one cell too late. Also reject
  `"   Context 78% used"` to prove a third indentation cell is not erased.

  Extend the existing placement tests with:

  ```rust
  vec![
      "› first".to_owned(),
      "  second".to_owned(),
      String::new(),
      "  Context 78% used".to_owned(),
  ]
  ```

  using the cursor on row zero at the rendered end, and:

  ```rust
  vec![
      "› pending".to_owned(),
      String::new(),
      "  Context 78% used".to_owned(),
      "  gpt-5.6-sol ultra · ~/project".to_owned(),
  ]
  ```

  as a status-looking popup followed by the real footer. Both must skip. Keep
  the documented pixel-identical single-row collision out of the negative
  matrix because the screen cannot distinguish it.

- [ ] **Step 5: Run the focused parser suite and confirm RED**

  ```bash
  cargo test --locked --lib codex_prompt::tests
  ```

  Expected failures include: context-left and leading-model rows are skipped;
  distinct weak combinations are skipped; partial context truncations are
  incorrectly captured; third-indent rows are incorrectly captured; and
  right-aligned indicators are skipped. Existing instructional-footer and
  prompt-byte tests must continue compiling.

- [ ] **Step 6: Implement the positioned candidate and supported-footer refinement**

  Change the capture call to pass pane width:

  ```rust
  let Some(_supported_footer) = parse_supported_codex_footer(
      rows,
      cursor_y,
      usize::from(metadata.width().get()),
  ) else {
      return unsupported_layout();
  };
  ```

  Define these private functions; do not retain a versioned wrapper or Boolean
  acceptance result:

  ```rust
  fn parse_supported_codex_footer(
      rows: &[VisibleRow],
      cursor_y: usize,
      pane_width: usize,
  ) -> Option<SupportedCodexFooter>;

  fn parse_positioned_footer_candidate(
      rows: &[VisibleRow],
      cursor_y: usize,
      pane_width: usize,
  ) -> Option<PositionedFooterCandidate<'_>>;

  fn parse_exact_instructional_footer(
      content: &str,
  ) -> Option<ExactInstructionalFooter>;

  fn parse_configured_footer(
      candidate: PositionedFooterCandidate<'_>,
  ) -> Option<ConfiguredFooterBasis>;
  ```

  `parse_positioned_footer_candidate` must locate the first nonempty row
  after at least one exactly empty inset row, reject any later nonempty row,
  strip exactly `TEXTAREA_MARGIN`, reject empty content and
  `content.starts_with(' ')`, and retain both the complete original row's
  display width and the pane width for right-zone geometry.
  `parse_supported_codex_footer` returns the exact instructional production
  first; otherwise it returns the configured basis.

- [ ] **Step 7: Implement right-zone stripping and configured segmentation**

  Add exact recognized indicator text in longest-first order. Strip it only
  when:

  1. `candidate.row_width == candidate.pane_width - 2`, using the complete
     original row width captured before removing `TEXTAREA_MARGIN`;
  2. the candidate content ends with that exact indicator;
  3. the preceding text ends with at least one ASCII space; and
  4. removing the complete ASCII-space gap leaves a nonempty left zone.

  Otherwise leave the candidate content unchanged. The configured parser then:

  - splits only on exact ` · `;
  - rejects empty segments;
  - rejects any U+2026 outside the last segment's final scalar;
  - when the last segment ends in U+2026, removes that entire segment from the
    evidence input; and
  - reconstructs no missing text.

  Use one helper returning a borrowed refined left zone, not a caller-visible
  indicator flag:

  ```rust
  struct ConfiguredStatusLeftZone<'a>(&'a str);

  fn configured_status_left_zone<'a>(
      candidate: &PositionedFooterCandidate<'a>,
  ) -> ConfiguredStatusLeftZone<'a>;
  ```

- [ ] **Step 8: Implement categorical evidence composition**

  Use this exact weak-state transition so family distinctness is structural:

  ```rust
  enum WeakEvidence {
      None,
      One(WeakSignalFamily),
      Corroborated(CorroboratedWeakSignals),
  }

  impl WeakEvidence {
      fn insert(self, next: WeakSignalFamily) -> Self {
          match self {
              Self::None => Self::One(next),
              Self::One(first) if first == next => Self::One(first),
              Self::One(first) => Self::Corroborated(CorroboratedWeakSignals {
                  _first: first,
                  _second: next,
              }),
              Self::Corroborated(proof) => Self::Corroborated(proof),
          }
      }

      fn finish(self) -> Option<CorroboratedWeakSignals> {
          match self {
              Self::Corroborated(proof) => Some(proof),
              Self::None | Self::One(_) => None,
          }
      }
  }

  impl ConfiguredFooterEvidence {
      fn finish(self) -> Option<ConfiguredFooterBasis> {
          match self {
              Self::High(signal) => Some(ConfiguredFooterBasis::High(signal)),
              Self::Weak(weak) => weak
                  .finish()
                  .map(ConfiguredFooterBasis::Corroborated),
          }
      }
  }
  ```

  A complete bounded context in any evidence segment constructs
  `ConfiguredFooterEvidence::High(HighTrustSignal::Context)`. A complete model
  selection in segment position zero constructs
  `ConfiguredFooterEvidence::High(HighTrustSignal::LeadingModel)`.
  Otherwise classify each complete segment into at most one
  `WeakSignalFamily`, insert it into `WeakEvidence`, wrap that state in
  `ConfiguredFooterEvidence::Weak`, and obtain `ConfiguredFooterBasis` only
  through `ConfiguredFooterEvidence::finish()`.

- [ ] **Step 9: Implement the exact lexical parsers**

  Use named `Option<MarkerType>` parsers for structured knowledge, not
  validate-and-discard Booleans:

  ```rust
  struct ModelSelection;
  struct ContextPercentage;
  struct CompactCount;
  struct StrictDottedVersion;
  struct CanonicalUuid;

  fn parse_model_selection(segment: &str) -> Option<ModelSelection>;
  fn parse_context_percentage(value: &str, suffix: &str)
      -> Option<ContextPercentage>;
  fn parse_compact_count(value: &str) -> Option<CompactCount>;
  fn parse_strict_dotted_version(value: &str)
      -> Option<StrictDottedVersion>;
  fn parse_canonical_uuid(value: &str) -> Option<CanonicalUuid>;
  fn parse_weak_signal_family(
      segment: &str,
      position: usize,
  ) -> Option<WeakSignalFamily>;
  ```

  Implement exactly:

  - Model token: `gpt-` plus one-or-more ASCII alphanumeric, `.`, `_`, or
    `-`, containing at least one ASCII digit. It may end there, or have one
    ASCII space plus
    `default|minimal|low|medium|high|xhigh|max|ultra`, optionally one ASCII
    space plus lowercase `fast`.
  - Context: exact `Context N% used` or `Context N% left`, where `N` is
    nonempty ASCII decimal and parses in `0..=100`.
  - Workspace: exact `~` or `/`, nonempty `~/suffix`, or nonempty
    absolute `/suffix`.
  - Compact count: ASCII integer optionally ending in uppercase
    `K|M|B|T`, or ASCII integer plus `.`, one-or-two ASCII fractional
    digits, and a required uppercase suffix. Accounting appends exact
    ` used| window| in| out`.
  - Runtime: exact
    `Starting|Ready|Working|Waiting|Thinking|Fast on|Fast off|raw output`.
  - Git: exact `PR #N`, `No changes`, or `+N -N` with nonempty ASCII
    digits.
  - Strict dotted version: exactly three nonempty ASCII decimal components
    separated by two dots, with no prefix or suffix.
  - Canonical UUID: ASCII hexadecimal groups of lengths `8-4-4-4-12` with
    exact hyphens and no surrounding text.

  Later model selections are Model weak evidence; a leading one is consumed as
  high evidence. Check weak families in the table order Model, Workspace,
  Accounting, Runtime, Git, Identity and return after the first match.

- [ ] **Step 10: Preserve exact instructional parsing as its own production**

  Replace `is_known_footer_hint` with parsers that return
  `ExactInstructionalFooter` or a private exact-hint marker. Preserve:

  ```text
  empty base for a bare bounded legacy percentage
  ? for shortcuts
  tab to queue
  tab to queue message
  Plan mode
  Plan mode (shift+tab to cycle)
  each nonempty shortcut/queue base followed by exact " · " and a Plan form
  any accepted base followed by ASCII-whitespace separation and N% context left
  ```

  Require the legacy percentage to be nonempty ASCII decimal in `0..=100`.
  Do not feed instructional fragments into configured weak-family composition.

- [ ] **Step 11: Format and run focused GREEN checks**

  ```bash
  cargo fmt --all
  cargo test --locked --lib codex_prompt::tests
  cargo test --locked --lib
  ```

  Expected: all parser matrices, existing 49-byte prompt fixture,
  empty-composer style proof, placement, privacy, and adjacent library tests
  pass. Inspect the implementation to confirm no `0145` production
  identifier and no loose numeric trust score remain.

- [ ] **Step 12: Commit the classifier task**

  ```bash
  git add src/codex_prompt.rs
  git commit -m "feat: classify Codex status footer evidence"
  ```

---

### Task 2: Integrate the Default Footer and Version-Neutral Warning

**Files:**

- Modify: `tests/capture.rs:165-172,450-477,533-565`
- Modify: `src/capture.rs:348-371`

**Interfaces:**

- Consumes: unchanged `CodexPromptAreaObservation` and
  `CodexPromptCaptureFailureKind::UnsupportedLayout`.
- Produces: the exact warning
  `visible pane does not match a supported Codex prompt layout`.
- Keeps the event type, automatic recovery payload, privacy assertions, and
  capture orchestration unchanged.

- [ ] **Step 1: Write the failing exact-warning assertion and update the integration footer**

  In
  `unstyled_single_row_text_retains_automatic_recovery_and_emits_one_safe_warning`,
  add:

  ```rust
  assert_eq!(
      failure.message(),
      "visible pane does not match a supported Codex prompt layout"
  );
  ```

  Keep the existing assertions that automatic Codex recovery has
  `prompt_area: None` and neither the failure nor event debug output contains
  the candidate text.

  Change only the final row in `captured_grid` to:

  ```rust
  "  gpt-5.6-sol ultra · /tmp/work"
  ```

  Add `assert!(result.events().is_empty());` to
  `exact_codex_capture_attaches_visible_prompt_input`. Leave
  `absent_grid` and `maximum_prompt_grid` on legacy instructional footers
  so those tests stay independent from configured-status recognition.

- [ ] **Step 2: Run the warning test and confirm RED**

  ```bash
  cargo test --locked --test capture unstyled_single_row_text_retains_automatic_recovery_and_emits_one_safe_warning
  ```

  Expected: FAIL because the current message contains
  `supported Codex 0.145.0 prompt layout`. The configured-default integration
  test should already pass through Task 1's classifier.

- [ ] **Step 3: Change only the unsupported-layout message**

  In `CodexPromptCaptureFailure::message`, replace the
  `UnsupportedLayout` arm with:

  ```rust
  CodexPromptCaptureFailureKind::UnsupportedLayout => {
      "visible pane does not match a supported Codex prompt layout"
  }
  ```

  Do not add a failure variant, attach row text, or change other messages.

- [ ] **Step 4: Run focused GREEN integration checks**

  ```bash
  cargo fmt --all
  cargo test --locked --test capture
  ```

  Expected: all capture integration tests pass, the default configured footer
  enriches the exact session with `draft\nsecond`, and unsupported layouts
  retain prompt-free automatic recovery with one sanitized warning.

- [ ] **Step 5: Commit the integration task**

  ```bash
  git add src/capture.rs tests/capture.rs
  git commit -m "fix: make Codex prompt layout checks version neutral"
  ```

---

### Task 3: Document the Invariant Classifier and Extension Discipline

**Files:**

- Modify: `docs/src/TOOL-RECOVERIES.md:131-187`
- Modify: `docs/src/ARCHITECTURE.md:401-433,1077-1078`

**Interfaces:**

- Consumes: the accepted July 29 design and the exact behavior implemented in
  Tasks 1-2.
- Produces: operator-facing recovery rules in `TOOL-RECOVERIES.md` and
  maintainer-facing module boundaries in `ARCHITECTURE.md`.
- Does not rewrite either historical design record or broaden
  `docs/src/DESIGN.md`.

- [ ] **Step 1: Rewrite only the configured-footer clauses in Visible Prompt Evidence**

  Preserve the frozen `132x40`, cursor `(9,37)`, seven-row, 49-byte fixture
  as baseline evidence. Replace the version-gate sentence and obsolete
  context-truncation footer bullet with prose containing this exact contract:

  ```markdown
  Codex 0.145.0 is the verified renderer baseline, not a runtime version gate.
  A compatible later renderer continues through the same visible grammar.

  A configured status footer requires the existing placement envelope plus
  either one complete high-trust signal or two distinct weak signal families.
  Complete Context N% used|left segments and a complete leading gpt- model
  selection are high trust. Later Model, Workspace, Accounting, Runtime, Git,
  and Identity forms are weak; repeated evidence from one family counts once.
  Exact instructional footers remain a separate production.
  ```

  Also state:

  - the exact two-space indent and ` · ` separator are syntax, not votes;
  - a recognized right-aligned Plan/IDE indicator is stripped only under exact
    text and geometry and contributes no trust;
  - a terminal `…` and its entire incomplete segment contribute no evidence;
    `Context N% u…` and `Context N…` no longer succeed alone;
  - footer classification is identical for theme-colored, all-faint, and
    unstyled text; faintness proves only an empty composer;
  - a single weak family, malformed value, or insufficient surviving evidence
    omits only `prompt_area`; and
  - layout drift uses the version-neutral prompt-free warning while exact
    session recovery remains available.

  Qualify multiline draft capture: the cursor must be on the final visible
  prompt row at its rendered end, or on the accepted empty trailing
  continuation.

- [ ] **Step 2: Update the architecture seam and maintenance rules**

  In the visible-grid/Codex parser section, document this refinement:

  ```text
  VisiblePaneGrid
    -> PositionedFooterCandidate
    -> ConfiguredFooterEvidence
    -> SupportedCodexFooter
    -> Absent | CapturedCodexPromptArea | CodexPromptCaptureFailure
  ```

  State that private constructors enforce the placement envelope and
  high-or-two-distinct-low composition; callers cannot receive trust counts or
  recombine predicates. Configured evidence reads only refined plain rows,
  while the existing opaque faint-suffix proof remains local to empty-composer
  classification.

  Rewrite `Maintaining renderer evidence` to require:

  1. one read-only captured compatibility fixture for each observed Codex
     update;
  2. no branch or rename when the existing grammar accepts that fixture;
  3. a family assignment, position rule, deduplication proof, and positive,
     collision, malformed, reordered, and truncated fixtures for a new signal;
  4. a distinct private policy only for an incompatible layout with separately
     recognizable invariants; and
  5. no runtime version dispatch, config reader, registry, numeric score, or
     generic renderer abstraction.

  Update the Verification Boundaries bullet to name configured-footer evidence
  composition, style independence, terminal truncation, and cross-version
  compatibility fixtures.

- [ ] **Step 3: Build the normative documentation and scan stale wording**

  ```bash
  mdbook build docs
  rg -n "tied to the observed Codex|Context N% u…|versioned policy|supported Codex 0\\.145\\.0 prompt layout|is_supported_codex_0145" \
    docs/src/TOOL-RECOVERIES.md docs/src/ARCHITECTURE.md src/codex_prompt.rs src/capture.rs
  git diff --check
  ```

  Expected: mdBook succeeds; the stale-pattern scan returns no matches; the
  July 28 historical design is deliberately excluded from the scan.

- [ ] **Step 4: Commit the documentation task**

  ```bash
  git add docs/src/TOOL-RECOVERIES.md docs/src/ARCHITECTURE.md
  git commit -m "docs: explain Codex footer trust composition"
  ```

---

### Task 4: Verify Exact Scope and Refresh Draft PR #2

**Files:**

- Verify only; modify no file unless a verification failure traces directly to
  Tasks 1-3.

**Interfaces:**

- Consumes: the three reviewed task commits plus the approved design and plan.
- Produces: a clean, pushed
  `feat/codex-visible-prompt-recovery` and refreshed draft PR #2.
- Does not merge the PR or publish a release.

- [ ] **Step 1: Run formatting and static analysis**

  ```bash
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features --locked -- -D warnings
  ```

  Expected: both commands exit zero with no warnings.

- [ ] **Step 2: Run the complete Rust test and documentation gates**

  ```bash
  cargo test --all-targets --all-features --locked
  cargo doc --all-features --locked --no-deps
  cargo package --locked
  mdbook build docs
  ```

  Expected: every command exits zero. Cargo may write only ignored
  `target/` artifacts; mdBook may write only ignored `docs/book/`.

- [ ] **Step 3: Verify deployable Mermaid assets**

  Run the same checks as `.github/workflows/docs.yml`: generated HTML must
  contain no `src=".assets/` reference, must contain at least one
  `src="assets/mermaid..."` reference, and every referenced file must exist
  under `docs/book/`.

  Expected: no hidden asset reference, a nonempty asset list, and no missing
  asset.

- [ ] **Step 4: Audit final bytes and scope**

  ```bash
  git diff --check origin/feat/codex-visible-prompt-recovery...HEAD
  git diff --name-status origin/feat/codex-visible-prompt-recovery...HEAD
  git status --short --branch
  rg -n "fn [A-Za-z0-9_]*145|struct [A-Za-z0-9_]*145|enum [A-Za-z0-9_]*145|type [A-Za-z0-9_]*145|supported Codex 0\\.145\\.0 prompt layout" \
    src tests docs/src docs/superpowers/specs/2026-07-29-codex-status-footer-trust-composition-design.md
  ```

  Expected: only the approved design, this plan, Task 1 parser, Task 2
  diagnostic/integration, and Task 3 normative docs differ from the remote
  branch; the worktree is clean; the stale production-name scan returns no
  matches. Baseline `0.145.0` references remain only where they identify
  fixture provenance.

- [ ] **Step 5: Obtain a final independent review**

  Dispatch a fresh read-only reviewer against exact `HEAD`. Require it to
  verify the approved high-or-two-distinct-low policy, refined-type boundary,
  right-indicator geometry, truncation neutrality, cross-version naming,
  diagnostic privacy, test coverage, and changed-file scope. Resolve every
  actionable finding with a new RED/GREEN cycle and rerun affected gates.

- [ ] **Step 6: Push and confirm the existing draft PR**

  ```bash
  git push origin feat/codex-visible-prompt-recovery
  gh pr view 2 --json number,isDraft,headRefName,baseRefName,url,headRefOid
  ```

  Expected: PR #2 remains a draft from
  `feat/codex-visible-prompt-recovery` into `main`, and `headRefOid`
  equals local `HEAD`.
