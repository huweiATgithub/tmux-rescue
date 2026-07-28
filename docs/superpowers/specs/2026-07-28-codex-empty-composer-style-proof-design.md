# Codex Empty Composer Style Proof Design

## Role

This follow-up design corrects two live Codex `0.145.0` renderer assumptions
in the visible-prompt recovery feature on PR #2. It supersedes only the empty
composer and footer-position clauses in
`2026-07-25-codex-visible-prompt-recovery-design.md`. The original capture,
snapshot, inspection, planning, restore, privacy, and exact-session contracts
otherwise remain authoritative.

## Goal

Recognize a normal empty Codex composer without warning only when tmux provides
independent styling evidence that the visible suggestion is renderer-owned
placeholder text. Also accept the normal composer when its recognized footer
is followed by unused blank terminal rows.

The change must not silently discard a real draft whose text happens to equal
a Codex suggestion and whose cursor has been moved to the textarea start.

## Live Evidence

A read-only snapshot against the installed `codex-cli 0.145.0` produced nine
unsupported-layout warnings for normal empty composers. Five rotating
suggestions were visible:

- `Implement {feature}`;
- `Use /skills to list available skills`;
- `Write tests for @filename`;
- `Explain this codebase`; and
- `Find and fix a bug in @filename`.

For each suggestion, `tmux capture-pane -p -e` showed:

- the `› ` or `» ` live prefix was not faint;
- every suggestion character was under SGR faint (`2`); and
- the cursor was at the two-cell textarea start.

A real pending draft captured from another pane was not faint and continued to
round-trip as five visible rows and 49 bytes.

Eight empty composers placed their footer on the final pane row. One placed the
same recognized footer after the normal blank inset and then left five terminal
rows blank. The plain-text parser rejected the first group because it knew only
one older placeholder string and rejected the last pane because it required the
footer to be the final physical row.

The separate case where Codex hosted an external editor without an opened
session file remains outside this change. Without an exact Codex session
identity, visible prompt capture is not attempted and recovery remains the
existing manual fallback.

## Decision

Read the visible pane once with `tmux capture-pane -p -e`. Refine that styled
terminal output at the visible-pane boundary into the existing plain rows plus
typed faint-text evidence. The Codex parser may return `Absent` only after it
receives such evidence for the exact suggestion suffix.

This is preferred over two alternatives:

1. A plain-string allowlist is smaller but unsafe. A renderer suggestion and a
   user typing the same text then moving the cursor to column two have identical
   plain rows and cursor metadata.
2. Suppressing or aggregating unsupported-layout warnings would reduce noise
   while hiding real prompt misses. Warnings remain part of the fail-closed
   contract.

One styled full-grid read is also preferred over separate plain and styled
reads. It binds displayed text and style to the same tmux observation, preserves
the existing metadata-before/after fence, and avoids a second content race.

## User-Visible Contract

A supported empty composer produces no `prompt_area` and no warning only when
all of these facts hold:

- the pane is outside copy mode and satisfies the normal composer grammar;
- the cursor is at the two-cell textarea start on the first and only prompt
  row;
- every character in the `› ` or `» ` prefix is effectively non-faint;
- the entire visible suffix after that prefix is faint; and
- the suffix exactly matches a supported Codex `0.145.0` suggestion.

The supported suggestions are the five live strings above plus the previously
documented `Ask Codex to do anything`. The older string remains supported only
with the same faint-style proof; plain text alone no longer establishes an
empty composer.

These cases remain `Skipped` and emit the existing prompt-free warning:

- a known suggestion without complete faint styling;
- an unknown faint string;
- a partially faint suggestion or one whose faint state is reset inside the
  text;
- nonempty visible text with the cursor at the textarea start;
- malformed or unsupported terminal attribute output; and
- any other unsupported composer, popup, or pane layout.

Real prompt capture remains style-agnostic after styled output is refined to
plain rows. A normal draft still requires the cursor at the visible end of its
last row and preserves the same visible text as before.

## Refined Visible Grid

`VisiblePaneGrid` remains the only value crossing `CaptureSource::read_visible_pane`.
Its constructor changes from a plain-output boundary to a styled-output
boundary, conceptually:

```rust
VisiblePaneGrid::try_from_tmux_styled_capture(
    metadata: VisiblePaneMetadata,
    output: Vec<u8>,
) -> Result<VisiblePaneGrid, VisiblePaneGridError>
```

The constructor consumes every terminal escape sequence and produces:

- the same control-free `VisibleRow` strings used by the prompt parser;
- row-count, terminal-width, and cursor proofs against the plain text; and
- an internal faintness map or set of ranges aligned with the refined row
  text.

Raw ANSI bytes, parser state, and unrefined style flags never leave
`visible_pane.rs`. `VisibleRow` exposes one atomic refinement operation,
conceptually:

```rust
fn faint_suffix_after_non_faint_prefix(
    &self,
    prefix: &str,
) -> Option<FaintVisibleText<'_>>
```

It returns `Some` only when the row starts with `prefix`, every character in
that prefix is effectively non-faint, the suffix is nonempty, and every
character in the suffix is effectively faint. `FaintVisibleText` is privately
constructed, borrows that exact suffix, and exposes only `as_str()`. The Codex
parser compares that proved text to the supported suggestion set. It cannot
obtain a detached style boolean or construct a proof for a different
substring.

## Styled Capture Grammar

The boundary accepts UTF-8 text plus seven-bit `ESC [` CSI SGR sequences
emitted by `tmux capture-pane -e`. It accepts those CSI sequences only when
their final byte is `m`, they contain no intermediate or private-marker bytes,
and their parameters form supported SGR operations. It rejects:

- non-SGR escape or CSI sequences;
- eight-bit C1 CSI bytes;
- truncated escapes;
- private, negative, nonnumeric, or otherwise malformed parameters;
- malformed extended-color parameter groups; and
- unknown SGR operations.

The decoder tracks effective intensity across characters and rows:

- reset (`0` or an empty SGR parameter list) clears faint;
- faint (`2`) sets faint;
- normal intensity (`22`) clears bold and faint; and
- supported unrelated color and text attributes leave faint unchanged.

SGR parameters are decimal integers separated by semicolons. The decoder
accepts exactly these operations:

- simple operations `0` through `29`, `30` through `37`, `39`, `40` through
  `47`, `49`, `50` through `55`, `59`, `60` through `65`, `73` through `75`,
  `90` through `97`, and `100` through `107`;
- extended-color heads `38`, `48`, and `58` only when immediately followed by
  either `5;n` or `2;r;g;b`; and
- an empty parameter list, with the same meaning as `0`.

Each indexed-color value and RGB component must be in `0..=255`. Extended-color
payload values are consumed as part of that operation and never interpreted as
standalone attributes. Colon-separated forms, internal empty parameters, and
every operation outside this finite vocabulary are rejected. Accepted
operations other than `0`, `2`, and `22` do not change the faint fact recorded
for this feature. Operations are applied from left to right: `2;22` ends
non-faint, while `22;2` ends faint. Style state may span a newline; row
boundaries do not implicitly reset it.

Styled-decoder errors are fixed, prompt-free variants. They may identify the
error class or row but never include the malformed sequence or adjacent pane
text.

After escape removal, the existing rules still reject plaintext controls,
malformed UTF-8, the wrong number of rows, rows wider than the pane in terminal
cells, and cursor coordinates outside the refined grid.

No new dependency is required. A small strict SGR decoder inside
`visible_pane.rs` is narrower than admitting a general terminal-state parser,
and `anstyle` remains an output emitter rather than an input authority.

## Footer Placement Grammar

After the cursor row, the supported bottom layout is exactly:

```text
one-or-more blank inset rows
one recognized Codex footer
zero-or-more blank terminal rows
```

The existing footer recognizer remains unchanged. The parser locates the first
nonempty row after the cursor, requires at least one blank row before it,
requires that row to be a recognized footer, and requires every later row to be
empty.

It rejects a footer immediately below the cursor, a missing footer, a second
footer, or any other nonempty row before or after the recognized footer. Moving
the footer does not widen what counts as a footer.

## Capture Flow And Failures

The source adapter keeps the current operation order:

```text
read exact pane metadata
  -> capture one full visible grid with `capture-pane -p -e`
  -> read exact pane metadata again
  -> require identical metadata and pane identity
  -> refine styled output into VisiblePaneGrid
  -> parse the Codex prompt area
```

The capture remains visible-only and does not request joined lines, history, or
explicit start/end rows. No tmux buffer or input command is introduced.

Malformed styled output, changed metadata, and unsupported layout continue to
remove only optional prompt enrichment. They retain exact automatic Codex
session recovery and emit a bounded warning that contains no prompt text or raw
escape sequence.

## Tests

### Styled Grid Boundary

Unit tests must prove:

- SGR is removed while plain row text, blank rows, Unicode width, and cursor
  geometry remain exact;
- the atomic row refinement proves only a fully non-faint prefix followed by a
  nonempty, fully faint suffix;
- reset and `22` end faintness at the correct character;
- faint state can span rows;
- malformed, truncated, non-SGR, private, unknown, and malformed extended-color
  sequences fail closed; and
- plaintext controls remain invalid after style decoding.

### Codex Parser

Parser tests must prove:

- each of the five live suggestions is `Absent` only when fully faint;
- the prior `Ask Codex to do anything` string follows the same rule;
- the same plain strings without faint proof are `Skipped`;
- partial faint styling and an unknown fully faint string are `Skipped`;
- the five-row, 49-byte real draft remains exact;
- a real draft and an empty composer accept both footer-last and
  footer-plus-trailing-blanks layouts; and
- no inset row, another nonempty row, or a second footer remains unsupported.

### Adapter And Capture

Fake-command tests must prove exactly one `capture-pane -p -e` occurs between
the unchanged metadata reads and that `-J`, history bounds, and every tmux input
command remain absent.

An isolated real-tmux test must render faint placeholder text and verify that
the adapter returns the same plain row plus faint proof. It uses only a unique
temporary `-S` socket.

Capture integration tests must prove a style-proven empty composer emits no
event, while the same text without proof retains automatic Codex recovery and
emits one prompt-free skip warning.

## Documentation

Implementation updates:

- `docs/src/ARCHITECTURE.md` for the styled-grid refinement boundary, named
  faint proof, capture command, and failure behavior; and
- `docs/src/TOOL-RECOVERIES.md` for the six faint-proven suggestions and the
  footer-plus-terminal-blanks grammar.

`docs/src/DESIGN.md`, the snapshot schema, inspect and plan output, restore
mechanics, and external-editor recovery do not change.

## Non-Goals

- Recovering a prompt held inside an external editor.
- Attaching prompt text to manual recovery when no exact Codex session exists.
- Parsing arbitrary terminal history or reconstructing hidden composer state.
- Treating color, bold, or suggestion text alone as proof of emptiness.
- Suppressing warnings for layouts whose prompt state remains ambiguous.
- Expanding support beyond the observed Codex `0.145.0` renderer contract.
