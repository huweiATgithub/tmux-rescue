# Codex Status Footer Trust Composition Design

## Role

This follow-up design extends configured status-footer recognition for visible
Codex prompt recovery on PR #2. It supersedes only the configured-footer
recognition and configured-footer truncation clauses in
`2026-07-28-codex-empty-composer-style-proof-design.md`.

The accepted empty-composer faint-suffix rule, exact Codex session identity,
visible prompt extraction, snapshot schema, capture and restore flow, privacy,
and best-effort downgrade behavior remain authoritative and unchanged.

## Goal

Recognize configured Codex status lines without maintaining one exhaustive
list of complete footer strings. Codex `0.145.0` is the verified evidence
baseline, not a runtime compatibility gate. A later Codex release that still
renders the accepted invariants uses the same classifier without a version
branch.

Recognition remains best-effort: a rejected footer removes only optional
prompt enrichment, while exact automatic Codex session recovery remains
available.

The classifier accepts a configured status row only when the existing layout
envelope holds and the visible row contains either:

- one high-trust textual signal; or
- two distinct low-trust textual signal families.

This is probabilistic renderer recognition, not proof of provenance. The
refined result is therefore named a supported footer rather than a proven
status line.

## Verified Renderer Baseline

The initial evidence comes from the exact Codex `rust-v0.145.0` renderer. That
renderer:

- accepts an ordered `[tui].status_line` array and defaults an unset array to
  `model-with-reasoning` followed by `current-dir`;
- preserves configured order and omits items whose values are unavailable;
- joins visible status values with exact ` · ` separators;
- renders either theme-colored items with faint separators or an entirely
  faint status line when `status_line_use_colors = false`;
- indents footer content by two terminal cells;
- may render a Plan-mode or IDE-context indicator in a separate right-aligned
  footer zone; and
- truncates the complete left status line from the right and appends U+2026
  `…` when it overflows.

The relevant upstream authorities at that tag are:

- `codex-rs/config/src/types.rs` for the configuration contract;
- `codex-rs/tui/src/chatwidget.rs` and
  `codex-rs/tui/src/chatwidget/status_surfaces.rs` for defaults, ordering,
  omission, and item text;
- `codex-rs/tui/src/bottom_pane/status_line_style.rs` for separators and
  styling; and
- `codex-rs/tui/src/line_truncation.rs` and
  `codex-rs/tui/src/bottom_pane/footer.rs` for truncation and placement.

The screen is the runtime authority. `tmux-rescue` does not read
`config.toml`: profiles, persisted session state, runtime omissions, and
configuration changes after launch make the visible row more relevant than a
separate configuration read.

These observations justify the initial grammar. Equality with the baseline
release number is not a compatibility or acceptance gate. A version-shaped
visible segment may contribute only the version-neutral weak Identity family
defined below. Compatibility is decided from the visible layout and signals.

## Decision

Keep the current public capture seam and replace only the private configured
footer predicate with an invariant-based evidence classifier. The classifier
has three stages:

```text
VisiblePaneGrid
  -> PositionedFooterCandidate
  -> ConfiguredFooterEvidence
  -> SupportedCodexFooter
```

`PositionedFooterCandidate` can be constructed only after the layout envelope
has been parsed. `ConfiguredFooterEvidence` classifies complete visible text
into named trust families. `SupportedCodexFooter` can be constructed only
from one high-trust signal or corroboration by two distinct low-trust
families.

Exact instructional footer forms remain a separate accepted production. They
must not be converted into weak configured-status fragments merely to share a
single scoring mechanism.

This design is preferred over three alternatives:

1. Numeric weighted scoring is compact but makes correlation and later score
   inflation easy to hide behind a threshold.
2. Parsing every `StatusLineItem` mirrors too much upstream implementation and
   still cannot prove the boundaries of user-controlled values.
3. Style-derived status recognition is not stable across
   `status_line_use_colors` and is not unique to configured status lines.

The selected categorical grammar gives one narrow extension seam without a
registry, policy trait, configuration reader, or new dependency.

## Cross-Version Compatibility

The classifier does not read `codex --version`, accept a version argument, or
branch on a detected release. Type and function names do not contain the
baseline version. A Codex update has three possible outcomes:

1. If the visible composer and footer still satisfy the accepted grammar,
   prompt capture continues unchanged.
2. If a new status item or ordering still leaves one high-trust signal or two
   distinct low-trust families visible, the row remains supported without a
   parser change.
3. If the layout or signals drift beyond the grammar, optional prompt capture
   is omitted with a version-neutral warning. Exact session recovery remains
   separate and available while its own identity contract still holds.

A stale classifier does not rewrite, complete, or reinterpret draft text. It
either captures the exact visible rows under the supported composer grammar or
stores no `prompt_area`. This fail-closed property does not remove the already
documented residual collision where a new renderer presents a non-footer row
with exactly the same accepted text and geometry.

## Structural Eligibility

The existing placement and composer grammar remains a mandatory gate. None of
these facts is a trust signal:

- the pane has already been classified as the exact supported Codex process
  and session;
- the pane is outside tmux copy mode;
- the prompt row independently satisfies the supported glyph, continuation,
  and cursor grammar;
- the first nonempty row after the cursor follows one-or-more exactly empty
  inset rows;
- that candidate is the sole remaining nonempty row, with zero-or-more empty
  terminal rows after it; and
- the candidate begins with exactly two ASCII spaces and no third leading
  space.

Before configured-status segmentation, the parser may remove one recognized
right-aligned indicator zone. The zone must end exactly two display cells
before the pane edge, be separated from the left status zone by at least one
ASCII space, and be exactly one of:

- `Plan mode`;
- `Plan mode (shift+tab to cycle)`;
- `IDE context`; or
- a Plan-mode form followed by exact ` · IDE context`.

The right zone is renderer placement syntax and contributes no trust. An
unknown right-aligned suffix is not stripped merely because it happens to end
at the expected cell.

An immediately adjacent candidate, another nonempty row before or after the
candidate, a second footer, or a detected popup remains unsupported. A
one-row popup or prompt continuation that produces exactly the same text and
geometry as a supported footer is observationally indistinguishable and is an
accepted residual collision under the best-effort contract.

## Configured Status Syntax

After removing the exact two-space indent and any recognized right indicator,
the nonempty left status zone is either one complete segment or multiple
nonempty segments separated by exact ` · `. Empty segments and alternate
separators are unsupported.

Configured values are joined without escaping. A path, title, headline, or
other user-controlled value can therefore contain the separator and imitate
multiple status fields. The classifier's families represent distinct lexical
evidence, not proven upstream item boundaries.

All structured numbers use ASCII digits. The classifier performs no Unicode
normalization and does not accept fullwidth digits or lookalike punctuation.
A strict dotted version is exactly three nonempty ASCII decimal components
separated by `.` with no sign, prefix, or suffix.

### Model Selection Grammar

A complete model-selection segment has:

1. one model token beginning with exact `gpt-`;
2. one-or-more ASCII alphanumeric, `.`, `_`, or `-` characters after that
   prefix, including at least one ASCII digit; and
3. either the end of the segment, or one ASCII space followed by one of the
   reasoning labels verified in the `0.145.0` baseline
   `default|minimal|low|medium|high|xhigh|max|ultra`, optionally followed by
   one ASCII space and exact lowercase `fast`.

Unknown provider names, custom reasoning labels, and other service-tier labels
do not match this initial grammar. Supporting one later requires an explicit
fixture and a deliberate trust-policy change.

### Compact Count Grammar

A complete compact count uses the renderer's nonnegative ASCII representation:
one-or-more digits optionally followed by uppercase `K`, `M`, `B`, or `T`, or
one-or-more digits, `.`, one-or-two digits, and one of those suffixes. The
accepted accounting productions append exact ` used`, ` window`, ` in`, or
` out` text. Multiple accounting productions still contribute only one
low-trust family.

## Trust Policy

### High-Trust Signals

One complete high-trust signal accepts an otherwise structurally eligible
configured footer:

1. `Context N% used` as one whole segment, with ASCII `N` in `0..=100`.
2. `Context N% left` under the same bounded grammar.
3. A complete model-selection segment in the first segment position.

Position is part of the model signal. The same complete model selection later
in the row is only low trust. A `gpt-...` substring inside an arbitrary segment
is not evidence.

Treating a leading model or complete context gauge as sufficient is a
deliberate probability choice approved for this best-effort feature. It makes
the verified baseline's unset default and useful single-item model/context
configurations recoverable without claiming arbitrary status-line support.

### Low-Trust Families

Two different families accept an otherwise structurally eligible configured
footer:

| Family | Complete productions |
| --- | --- |
| Model | The model-selection grammar in any segment after the first |
| Workspace | Exact `~` or `/`, nonempty `~/...`, or nonempty absolute `/...` segment |
| Accounting | Compact count followed by `used`, `window`, `in`, or `out` |
| Runtime | Exact `Starting`, `Ready`, `Working`, `Waiting`, `Thinking`, `Fast on`, `Fast off`, or `raw output` |
| Git | Exact `PR #N`, `No changes`, or `+N -N`, using nonempty ASCII digit sequences |
| Identity | A strict three-component ASCII dotted version or a canonical hyphenated ASCII UUID segment |

Repeated productions from one family count once. In particular:

- each complete segment contributes at most one low-trust family;
- model name and reasoning in one segment are one Model signal;
- several token counters are one Accounting signal;
- `Fast on` and `raw output` are one Runtime signal; and
- a separator, the two-space indent, last-row placement, or an ellipsis never
  becomes an additional signal.

Opaque complete segments may accompany sufficient evidence without
contributing trust. Malformed near-matches contribute no signal. A structural
syntax violation, such as an empty ` · ` segment, rejects the candidate rather
than being ignored.

Conceptually, the private proof-bearing types are:

```rust
enum SupportedCodexFooter {
    Instructional(ExactInstructionalFooter),
    Configured(ConfiguredFooterBasis),
}

enum ConfiguredFooterBasis {
    High(HighTrustSignal),
    Corroborated(CorroboratedWeakSignals),
}

struct CorroboratedWeakSignals {
    // Private representation whose constructor requires at least two
    // distinct WeakSignalFamily values.
}
```

Callers receive only `SupportedCodexFooter`; they cannot obtain raw trust
counts or recombine individual predicates.

## Instructional Footers

The existing exact shortcut, queue, and Plan-mode footer grammar remains
accepted independently, including its bounded legacy `N% context left`
production. This text is renderer instruction content rather than a configured
status-item list.

The configured `Context N% left` item and the legacy `N% context left` footer
remain separate productions because their word order and renderer paths are
different.

## Truncation

U+2026 `…` is a neutral terminal truncation marker, never evidence. After any
recognized right indicator is removed, when the left status zone ends in it:

- complete segments before the incomplete terminal fragment may contribute
  normally;
- the incomplete terminal fragment contributes no high- or low-trust signal;
- no missing suffix or status item is reconstructed; and
- an ellipsis outside the terminal fragment, or any visible text after it,
  remains unsupported.

This intentionally supersedes the earlier rule that accepted terminal
`Context N% u…` or `Context N…` by itself. Such a row remains accepted when
another complete high signal or two complete low families survive before the
truncation. A single partial context or model fragment now fails closed.

Examples:

```text
accepted: Context 78% used
accepted: gpt-5.6-sol ultra
accepted: gpt-5.6-sol ultra · ~/projects/tmux-rescue
accepted: ~/projects/tmux-rescue · gpt-5.6-sol ultra
accepted: Fast on · 258K window
accepted: Context 78% used · ~/very/long/path…

rejected: ~/projects/tmux-rescue
rejected: main · gpt-5.6-sol ultra
rejected: prose mentioning gpt-5.6-sol ultra
rejected: 258K window · 2.55M used
rejected: Context 78% u…
rejected: gpt-5.6…
```

The later model in `main · gpt-5.6-sol ultra` supplies only the Model family,
so it is insufficient alone. The two accounting segments in
`258K window · 2.55M used` remain one family.

## Style Independence

Configured-footer trust uses refined plain text only. Faintness remains a
private input solely to the existing empty-composer proof.

Footer recognition must behave identically for the same visible text when
items are theme-colored, entirely faint, or represented without SGR in a unit
fixture. No foreground-color map, raw SGR query, Ratatui dependency, or generic
terminal-style interface is added.

This makes `status_line_use_colors` irrelevant to configured-footer
recognition while avoiding an incomplete style heuristic.

## Capture Flow And Failures

The source adapter, metadata fence, and common caller remain unchanged:

```text
read exact pane metadata
  -> capture one styled visible grid
  -> read exact pane metadata again
  -> require identical metadata and pane identity
  -> refine VisiblePaneGrid
  -> parse a supported footer
  -> parse the visible prompt area
```

A candidate with insufficient complete evidence, malformed status syntax, or
an unsupported truncation returns the existing prompt-free unsupported-layout
failure. It does not expose captured text, alter the exact Codex recovery
identity, or downgrade the pane from automatic session recovery. Only optional
`prompt_area` enrichment is omitted.

No new public error variant is required. Private mismatch reasons may be used
in tests but must not retain row text or enter snapshot diagnostics.

The unsupported-layout diagnostic is version-neutral, for example:
`visible pane does not match a supported Codex prompt layout`. It must not
claim that the running Codex release is `0.145.0`.

## Tests

Tests exercise `capture_visible_codex_prompt` through the same interface used by
capture orchestration. Private signal parsers may have focused boundary tests,
but production callers do not receive those seams.

### Positive Matrix

The parser tests must cover:

- `Context 0% used`, `Context 100% used`, `Context 0% left`, and
  `Context 100% left` as high signals;
- a single complete leading model and the verified `0.145.0` baseline default
  model-plus-directory order;
- reordered directory-plus-model as Workspace plus later Model;
- Runtime plus Accounting, and Git plus Identity using a non-baseline strict
  dotted version such as `0.146.0`, as corroboration;
- arbitrary opaque segments alongside sufficient evidence;
- complete evidence before a terminally truncated fragment;
- a context-only high signal with a recognized right-aligned Plan or IDE
  indicator;
- identical visible footer text with theme-color-like SGR, all-faint SGR, and
  no SGR; and
- the existing live five-row, 49-byte prompt fixture without changing its
  extracted bytes.

### Negative Matrix

The parser tests must reject:

- one later model, one path, or any other single low-trust family;
- a model-like substring that is not a complete segment;
- repeated low-trust Model, Accounting, Runtime, or other same-family
  productions when no high signal is present;
- rows whose only would-be high signal is an invalid or non-ASCII context
  percentage;
- rows whose claimed weak families depend on malformed model or compact-count
  productions;
- a malformed dotted version paired with only one other weak family, including
  `v0.146.0`, `0.146`, `0.146.0-beta`, `0..146`, or non-ASCII digits;
- empty segments, alternate separators, and nonterminal ellipses;
- a row supported only by `Context N% u…`, `Context N…`, `C…`, or a truncated
  model;
- a status-looking popup followed by a real footer;
- a cursor moved upward in a multiline draft; and
- the existing copy-mode, shell-mode, duplicate-row, and cursor-alignment
  failures.

The cursor-up and popup fixtures must include the real footer or other visible
evidence needed to distinguish them. A pixel-identical one-row replacement is
the documented residual collision, not a testable negative production.

### Test Locality

Replace the existing context-truncation-only table with the trust-composition
matrices rather than adding overlapping cases. Keep blank-inset placement tests
focused on placement using one canonical accepted footer. Capture integration
needs one default-footer happy path; the full trust matrix remains in the Codex
parser tests.

## Documentation

Implementation updates:

- `docs/src/TOOL-RECOVERIES.md` will describe the high-or-two-distinct-low
  configured-footer contract, style independence, terminal truncation, and
  intentional false negatives;
- `docs/src/ARCHITECTURE.md` will identify the private evidence-classifier seam
  and its cross-version maintenance rules; and
- the older July 28 design record remains historical and is superseded by this
  document rather than rewritten.

`docs/src/DESIGN.md`, visible-grid refinement, snapshot data, inspect output,
restore mechanics, and external-editor behavior do not change.

## Extension Discipline

Keep the first implementation in `codex_prompt.rs`, grouped into layout,
evidence extraction, composition policy, and tests. Adding one signal requires:

1. assigning it to an existing or explicitly new trust family;
2. documenting whether position changes its trust;
3. proving it does not double-count an existing family; and
4. adding positive, single-signal collision, malformed, reordered, and
   truncated fixtures as applicable.

A Codex update requires one read-only captured compatibility fixture. If the
existing grammar accepts it, the version change requires no parser branch or
renaming. If it does not, extend the existing grammar only with corresponding
positive and collision fixtures. Introduce a distinct internal policy only
when an incompatible layout has its own safely recognizable invariants.

Do not add a numeric score, caller-supplied registry, configuration reader,
release-number dispatch, renderer trait, or separate module merely because the
Codex version changed.

## Non-Goals

- Recognizing every legal `[tui].status_line` configuration.
- Guaranteeing compatibility with an unknown future renderer layout.
- Authenticating renderer provenance from terminal pixels.
- Reading or dispatching on the installed Codex version.
- Reading or reproducing active Codex configuration.
- Treating arbitrary branch, project, title, headline, permission-profile, or
  service-tier text as evidence.
- Recovering hidden, scrolled-out, popup-covered, or external-editor input.
- Using style as required or decisive configured-footer evidence.
- Changing empty-composer classification, prompt serialization, session
  resolution, snapshot schema, restore behavior, or diagnostic structure.
