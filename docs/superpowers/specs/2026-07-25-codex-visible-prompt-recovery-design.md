# Codex Visible Prompt Recovery Design

## Goal

Preserve the visible unsent Codex prompt area in a tmux-rescue snapshot and
prepare that text after the exact Codex session is restored. This protects
follow-up notes typed while an agent is still working without submitting or
interrupting the running agent.

The feature is deliberately best effort. tmux-rescue records the text cells
that Codex currently renders in its prompt area; it does not claim access to
Codex's complete internal composer state.

## User-Visible Contract

`tmux-rescue snapshot` automatically attempts prompt-area capture for every
pane whose foreground process has already resolved to an exact Codex session.
There is no new flag.

When capture succeeds, the snapshot stores every visible prompt-area row in
display order. Restore:

1. launches `codex resume <session-id>` through the existing automatic
   recovery path;
2. verifies that the restored foreground Codex process owns that exact
   session ID; and
3. bracketed-pastes the captured prompt-area text without pressing Enter.

The restored pane therefore contains editable pending text. tmux-rescue never
submits it.

The visible representation is authoritative for this feature:

- explicit blank rows and indentation are preserved;
- each displayed row boundary becomes `\n` in the captured value;
- a rendered `[Pasted Content N chars]` token is preserved literally;
- the hidden payload represented by that token is not expanded or recovered;
- visual soft wraps cannot be distinguished from user-entered newlines and
  therefore also become `\n`; and
- rows scrolled out of the Codex textarea are not captured.

After restore, a literal `[Pasted Content N chars]` token is ordinary composer
text. Codex no longer associates it with the original hidden paste payload.

Capture reflects one explicit snapshot invocation. This feature adds no
periodic scheduler, hook, or crash-time capture. A draft typed after the most
recent snapshot is not recoverable from that snapshot.

## Observed Feasibility

A live Codex 0.145.0 pane was captured read-only through tmux. Its prompt glyph
was at zero-based row 33 and its cursor was at row 37. Removing the two-column
Codex live prefix reconstructed five visible rows exactly:

```text
The test prompt for recovering.

Line 1.

Line 2.
```

Literal tmux bracketed paste into the same composer was also verified without
submitting the prompt. The probe text was then removed and the original draft
was preserved.

Codex keeps a large paste's hidden value in its internal `pending_pastes`
state and renders only a placeholder in the textarea. tmux exposes the
placeholder cells, not that hidden value. This is why the contract names the
captured fact a visible prompt area rather than a complete draft.

## Snapshot Model

Prompt-area state belongs only to automatic Codex recovery. Nest it in that
variant so a Claude, server, manual-command, idle, or unavailable pane cannot
carry Codex prompt text:

```text
AutomaticRecovery::Codex {
  session_id: CodexSessionId,
  prompt_area: Option<CapturedCodexPromptArea>
}

CapturedCodexPromptArea {
  text: CapturedPromptText
}
```

`CapturedPromptText` is an opaque refined value. Its constructor proves:

- valid UTF-8;
- at least one non-whitespace displayed character;
- `\n` is the only permitted control character;
- no carriage returns or terminal escape characters;
- no more than 16 KiB after UTF-8 encoding.

Live capture constructs this value only through the prompt-area parser. Loading
an untrusted snapshot can re-establish the text invariants above, but cannot
prove that the serialized value originally came from a live Codex screen.

The visible row count is derived from `text`; it is not stored as a second
fact that could disagree.

The raw Codex recovery object gains an optional `prompt_area` field. Serde
defaults an absent field to `None` and omits `None` while serializing. New
binaries therefore read existing snapshots unchanged. Existing binaries keep
their current `deny_unknown_fields` behavior and reject a new snapshot that
contains prompt data rather than silently ignoring it. This feature does not
add schema versioning or migration.

## Capture Architecture

The existing `CaptureSource` seam gains one read-only visible-pane operation.
Its tmux adapter targets a parsed, ephemeral `TmuxPaneId`; pane IDs remain
capture-time handles and are never serialized. Including that ID in
`TopologyPane` also makes a replaced pane fail the existing before/after
topology equality check.

Conceptually, the adapter returns:

```text
VisiblePaneGrid {
  width: PaneWidth,
  height: PaneHeight,
  cursor: VisibleCellPosition,
  rows: ExactHeight<Vec<VisibleRow>>
}
```

Construction parses and refines tmux output at the adapter seam. It rejects
invalid geometry, an out-of-range cursor, malformed UTF-8, or a row count that
does not equal the reported pane height.

For an exact Codex pane, the adapter samples pane ID, dimensions, cursor, and
mode immediately before and after `capture-pane -p`. The samples must match.
`capture-pane -J` is not used: Codex's ratatui renderer writes its own visual
rows, so tmux's terminal-wrap joining cannot recover Codex logical lines.

Matching metadata cannot prove that no character changed between commands.
The remaining race is part of the best-effort contract. A structurally invalid
or visibly changing capture is skipped rather than guessed.

A private pure prompt-parser module presents one interface:

```text
capture_visible_codex_prompt(VisiblePaneGrid)
  -> CodexPromptAreaObservation

CodexPromptAreaObservation =
    Absent
  | Captured(CapturedCodexPromptArea)
  | Skipped(CodexPromptCaptureFailure)
```

The parser owns all knowledge of the supported Codex screen grammar. Capture
orchestration and snapshot serialization do not know about glyphs, margins,
cursor cells, footer rows, or placeholder strings.

## Prompt-Area Grammar

The first version accepts only the normal Codex composer layout observed in
Codex 0.145.0:

- `›` and `»` are recognized prompt glyphs; shell mode's `!` is not;
- the prompt glyph occupies the first display cell and the textarea begins
  after Codex's two-cell live prefix;
- the prompt area is followed by the normal blank bottom inset and one-line
  footer;
- the cursor is on the final visible prompt row and at that row's visible end;
- every continuation row is empty after tmux trimming or begins with the same
  two-cell textarea margin; and
- no popup or unrecognized bottom-pane layout intersects the candidate rows.

Requiring the cursor at the visible end matches the pending-follow-up use case
and prevents an empty composer's dim placeholder from being mistaken for user
text. A user who moved the cursor into the middle of a draft gets a skipped
capture, not a partial value presented as complete visible input.

The parser removes the two renderer-owned prefix cells from each accepted row,
preserves all remaining leading indentation, joins the rows with `\n`, and
constructs `CapturedPromptText`. An empty trimmed terminal row becomes an empty
prompt line. If tmux trimmed significant trailing spaces, the cursor no longer
matches the visible row end and the parser skips the capture rather than
silently deleting them.

The parser does not special-case `[Pasted Content N chars]`; it is ordinary
visible text and is stored verbatim.

If a large draft has scrolled within the textarea, Codex provides no visible
completeness marker. The parser may therefore capture only the visible suffix.
The type and all user-facing text call the result a visible prompt area and do
not imply hidden rows were recovered.

## Capture Flow And Failures

Pane capture remains topology-first:

```text
stable topology candidate
  -> inspect foreground process
  -> classify exact Codex session
  -> read and refine VisiblePaneGrid
  -> parse CapturedCodexPromptArea
  -> attach it to that Codex recovery variant
  -> validate and publish the immutable snapshot
```

Prompt capture is optional enrichment. `Absent` stores no prompt field and is
not a warning. A tmux read failure, changing metadata, unsupported layout,
unsafe text, or oversized prompt produces a bounded
`CodexPromptCaptureFailure` event and stores no prompt field. It does not
downgrade or discard otherwise valid Codex session recovery.

Failure text is terminal-safe and never includes the captured prompt content.

## Inspection And Planning

Human-facing output acknowledges the captured fact without echoing potentially
sensitive draft text.

For a Codex pane with pending input, `tmux-rescue inspect` adds a line such as:

```text
pending input  5 visible rows · 49 bytes
```

Counts are derived from the validated value. The aggregate program summary is
unchanged.

Restore plan output adds a post-recovery statement for that pane:

```text
after recovery  paste 5 visible rows without Enter
```

Neither view prints the prompt text. A user who needs the literal stored value
can inspect the owner-only snapshot JSON.

Planning carries prompt input only on a Codex automatic launch whose expected
session ID is the ID enclosing that prompt in the validated snapshot. The
opaque plan constructor prevents other automatic recovery variants or fallback
actions from acquiring it.

## Restore Architecture

The existing automatic launch remains unchanged through session verification.
Prompt restoration begins only after `observe_automatic` has confirmed the
exact expected Codex session.

The recovery-target seam gains one deep operation conceptually equivalent to:

```text
paste_codex_prompt_area(
  pane: SourcePaneCoordinate,
  expected: CodexSessionId,
  input: CapturedCodexPromptArea
) -> CodexPromptPasteResult
```

This operation owns both the fresh identity check and the paste. It does not
return a reusable "Codex verified" token whose proof could go stale between
calls.

Immediately before sending input, the tmux adapter:

1. re-observes the Linux foreground process tied to the restored pane;
2. classifies it as the exact expected Codex session;
3. rechecks restore-server ownership, pane liveness, and the restored pane PID
   in the tmux-side conditional; and
4. runs only `set-buffer` followed by `paste-buffer -d -p -r`.

It never adds `send-keys Enter` for the prompt area. The input uses the same
bounded literal tmux-buffer mechanism already used by paste-only recovery
hints, but it has a Codex-running guard rather than a shell-foreground guard.

The Linux observation and tmux conditional cannot form one atomic process
lock. A foreground transition in their scheduling gap remains possible, as it
does at the existing guarded input seam. The operation minimizes the gap,
never reuses the earlier settle observation, and fails closed on every
detectable mismatch.

Restore outcomes distinguish:

- automatic recovery with no captured prompt;
- automatic recovery with the prompt area prepared;
- automatic recovery succeeded but prompt preparation needs attention; and
- automatic recovery itself failed or became a paste-only command hint.

If automatic recovery does not reach the exact Codex session, tmux-rescue
sends no prompt text. If session recovery succeeds but prompt preparation is
blocked or fails, the target is retained, the pane is reported partial, and
the prompt remains available in the immutable snapshot. There is no automatic
retry and no attempt to paste it into the shell fallback.

## Security And Privacy

Prompt text may contain secrets. It is stored as plaintext inside the existing
owner-only immutable snapshot and follows the existing no-automatic-deletion
policy. The feature adds no separate encryption, redaction, retention, or
opt-out setting.

Snapshot and plan diagnostics report only row and byte counts. Capture-failure
events do not include prompt text. Snapshot JSON remains untrusted input on
load; raw prompt text must pass refinement before inspection or planning.

## Documentation Changes During Implementation

Implementation must synchronize the authoritative docset:

- `DESIGN.md` adds visible Codex prompt areas to preserved state and narrows
  the unsaved-input non-goal to hidden or unsupported input;
- `ARCHITECTURE.md` owns the refined types, parser and adapter seams, capture
  flow, planning rule, guarded post-recovery paste, outcomes, limits, and
  snapshot compatibility; and
- `TOOL-RECOVERIES.md` owns the Codex 0.145.0 renderer evidence, literal
  placeholder behavior, and exact-session post-recovery rule.

The mdBook summary needs no new page because these are changes to existing
authorities.

## Verification Strategy

### Pure Parser Tests

Fixture tests cover:

- both `›` and `»` prompt glyphs;
- the observed five-row prompt with two blank rows;
- leading indentation and a trailing empty prompt row;
- literal `[Pasted Content N chars]` preservation;
- visual wrapped rows becoming newline-separated text;
- a scrolled visible suffix without claiming completeness;
- empty composer placeholders;
- cursor-not-at-end, popup, footer, prefix, and unknown-glyph rejection;
- tmux-trimmed trailing-space ambiguity;
- invalid geometry, malformed UTF-8, forbidden controls, and the 16 KiB bound;
  and
- stable derivation of visible row and byte counts.

### Model And Capture Tests

Tests prove:

- existing snapshots deserialize with no prompt area;
- a new Codex prompt area round-trips exactly;
- prompt text cannot be attached to a non-Codex recovery variant;
- raw unsafe or oversized prompt text cannot reach `ValidatedSnapshot`;
- only an exact Codex classification requests a visible-pane capture;
- `Absent` and `Skipped` leave valid Codex recovery intact;
- an ephemeral tmux pane ID participates in topology equality but is never
  serialized; and
- adapter commands use `capture-pane -p`, target the parsed pane ID, perform no
  pane mutation, and reject changing metadata.

### Planning, Restore, And Output Tests

Tests prove:

- only the matching Codex launch carries the post-recovery prompt action;
- preflight fallback and failed automatic recovery never paste prompt text;
- exact Codex recovery is re-observed before prompt preparation;
- successful preparation emits `set-buffer` and
  `paste-buffer -d -p -r` with the exact multiline bytes and no Enter command;
- identity, ownership, pane-liveness, or process-observation failure sends no
  prompt input and produces a partial outcome;
- inspection and plan output show counts and never the captured text; and
- all existing recovery variants retain their current behavior.

Repository verification includes focused tests, the full Rust test suite,
`cargo fmt --check`, strict Clippy, build, documentation tests, and
`mdbook build docs`.

## Rejected Alternatives

### Single-Row Capture

This was initially proposed for simplicity, but the live pane demonstrated
that tmux preserves a normal multiline Codex prompt area, including blank
rows. Restricting capture to one row would discard useful visible state without
removing the fundamental hidden-state limitation.

### Expanding Large-Paste Placeholders

The full pasted payload exists only in Codex's internal composer state.
Recovering it would require a Codex-native persistence or export interface.
Screen scraping cannot obtain it, so v1 preserves the rendered token literally.

### Transcript-Tail Reminder

A generic pane tail mixes agent output, status lines, and prompt text. It is
useful as human evidence but is not safe input for automatic preparation.
The prompt-area grammar gives this feature a narrower, typed fact.

### Codex Process Memory Or Terminal-Stream Reconstruction

Reading process memory or replaying raw terminal traffic would be brittle,
privacy-invasive, version-sensitive, and substantially more complex. Neither
is justified for a best-effort recovery aid.

## Acceptance Criteria

The feature is complete when a snapshot of the demonstrated multiline Codex
pane records exactly:

```text
The test prompt for recovering.

Line 1.

Line 2.
```

and a fresh-target restore resumes the exact captured Codex session and places
that text back in its composer without submitting it. Unsupported or ambiguous
screens must preserve normal Codex session recovery, send no guessed prompt
text, and report the skipped or partial state without exposing the draft.
