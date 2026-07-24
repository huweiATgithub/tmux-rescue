# Snapshot Inspection Design

## Goal

Add a read-only `tmux-rescue inspect` command that validates a snapshot and
renders the workspace it contains as a polished terminal tree. Inspection uses
the global `latest` snapshot by default and accepts an explicit immutable
snapshot path when requested.

The view is for workspace recall. It presents captured sessions, windows,
panes, working directories, programs, commands, and capture limitations in
user-facing language. The renderer does not generate recovery-planner
terminology such as `Automatic` or `Manual`.

## Command Surface

```text
tmux-rescue inspect [SNAPSHOT] [--color <auto|always|never>]
```

- With no `SNAPSHOT`, inspection uses the existing global `latest` selection
  and validation contract.
- With `SNAPSHOT`, inspection uses the existing explicit-path loader and does
  not require a state-root environment.
- `--color` defaults to `auto`.
- Inspection never contacts tmux, inspects processes, plans a restore, or
  performs preflight against the current machine.

A successful inspection exits `0`. Selection, loading, validation, rendering,
or output failure exits `1`. There is no partial-success exit status for
inspection: unstable topology and unavailable pane program information are
valid snapshot facts and do not prevent the tree from being displayed.

## Output Role

Successful output is one self-contained document on standard output:

1. snapshot identity and capture metadata;
2. aggregate workspace counts;
3. a complete ordered session/window/pane tree.

Failures that prevent a document from being produced leave standard output
empty and report the error on standard error. Inspection emits no progress
messages.

The default view is Unicode and never truncates stored values. Long values may
wrap naturally in the terminal. An ASCII mode, paging, filtering, alternate
views, and machine-readable output are not part of this feature.

## Visualization

The structural grammar is:

```text
Snapshot     latest
Captured     2026-07-24T05:31:32.581307924Z
Source       /tmp/tmux-1000/default
Consistency  ● stable topology
File         /home/huwei/.local/state/tmux-rescue/snapshots/<full-name>.json

Contents     6 sessions · 15 windows · 15 panes
Programs     11 Codex · 3 shells · 1 tmux-rescue

◆ MetaNC · 3 windows · 3 panes
  cwd /home/huwei/projects/MetaNC
├─ [0] node
│  └─ [0] Codex
│          session 019f7ac5-a55c-7e70-8b31-872ae70c9a94
│          cwd = session
├─ [1] zsh
│  └─ [0] shell
│          cwd = session
└─ [2] zsh
   └─ [0] ! program not captured
           reason foreground process disappeared
           cwd /home/huwei/projects/MetaNC/.worktrees/ci-dag-readme
```

The sample is shown without ANSI escapes. Color applies only to the semantic
tokens defined below.

### Header

`Snapshot` is `latest` when selection used the global pointer and `explicit`
when the user supplied a path. `File` shows the complete selected path returned
by `LoadedSnapshot`, without `~` abbreviation or ellipsis. The global-latest
path is absolute because it comes from the absolute state root; an explicit
relative path remains relative and is not silently canonicalized for display.

`Captured` preserves the snapshot's exact validated RFC 3339 representation,
including fractional precision and offset. `Source` displays the recorded tmux
socket path.

`Consistency` describes topology consistency specifically:

- `● stable topology`; or
- `▲ unstable topology after N attempts`.

An unstable snapshot remains a valid snapshot. The warning is prominent in the
header, the complete snapshot tree follows it, and inspection exits `0`. The
inspector cannot show the original retry events because the snapshot persists
only the exhausted attempt count, not those capture events.

### Summaries

`Contents` counts sessions, windows, and panes from the validated tree.

`Programs` aggregates pane facts by visible program identity in first-seen
tree order:

- Codex becomes `Codex`;
- Claude Code becomes `Claude Code`;
- an idle pane becomes `shell` or `shells`;
- a captured command uses the safely displayed basename of its captured
  executable, falling back to the full executable when it has no basename; and
- unavailable program information becomes `not captured`.

Entries with the same visible identity are counted together. Summary values
are not truncated. If the line exceeds the terminal width, the terminal may
wrap it naturally.

### Tree

Sessions, windows, and panes retain validated snapshot order.

Each session root contains:

- the session name;
- its window and pane counts; and
- its full recorded working directory.

Each window node contains its source index and name. Each pane node contains
its source index and the captured program fact:

- `Codex`, followed by `session <id>`;
- `Claude Code`, followed by `session <id>`;
- the complete captured command, followed by `executable <value>`;
- `shell`; or
- `! program not captured`, followed by `reason <failure>`.

Every pane also shows its recorded working directory. When its path bytes are
exactly equal to the containing session's path bytes, the renderer writes
`cwd = session`; this is display compression, not cwd inheritance. A differing
pane path is always shown in full.

The renderer never generates or color-codes `Automatic`, `Manual`, or
equivalent recovery-planner classifications. Codex, Claude Code, recognized
serve commands, and other captured commands are all presented as facts stored
for the pane. User-controlled names and command arguments remain complete even
when their literal content includes words such as `automatic` or `manual`.

## Palette

The palette uses standard ANSI named colors and the terminal's default
foreground. It does not use RGB colors, backgrounds, bright variants, dim or
faint text, italics, underlining, or colored tree connectors.

```text
Cyan    ◆       session anchor
Green   ●       stable-topology marker
Yellow  ▲       unstable-topology marker
Yellow  !       program-not-captured marker
Red     error:  fatal inspection failure only
```

The selected snapshot, session names, window names, pane program or command,
and explicit warning phrase use bold default foreground. Other labels, values,
paths, identifiers, counts, indexes, reasons, and connectors use normal default
foreground.

Only the listed marker or prefix receives its color. For example, the dot is
green while `stable topology` remains normal foreground. This limits contrast
risk across dark and light terminal themes. It also makes color redundant:
removing every ANSI sequence leaves identical text, glyphs, ordering, and
meaning.

`--color auto` uses color only when the output stream supports it and honors
standard environment conventions such as `NO_COLOR` and `CLICOLOR`.
`--color always` preserves ANSI styling through redirection, and
`--color never` emits no ANSI sequences. An explicit option takes precedence
over automatic environment and terminal detection.

Untrusted snapshot text is escaped before renderer-owned ANSI sequences are
added. User-controlled bytes can never introduce styles or terminal control
sequences.

## Value Rendering

Snapshot names are already validated as control-free UTF-8. Lossless operating
system values may contain non-UTF-8 bytes and require a separate display
encoding.

Paths, executable values, and arguments preserve printable Unicode. Literal
backslashes and quotation marks are escaped, control characters are rendered
visibly, and non-UTF-8 bytes use `\xNN`. The encoding is unambiguous: a literal
backslash is doubled, so it cannot be confused with an escaped byte.

Commands preserve argv boundaries. A simple argument is displayed bare; an
empty argument or an argument containing whitespace, quotation marks,
backslashes, controls, or non-UTF-8 bytes is double-quoted with the display
escapes above. This is a diagnostic representation only. Inspection does not
claim that copying the displayed line is safe to execute in an arbitrary
shell.

All value rendering is complete within the existing validated snapshot size
bounds. Inspection adds no terminal-width or diagnostic-length truncation.

## Module Design

Snapshot loading remains in the reusable library. Human presentation remains
in the binary crate, consistent with the existing architecture contract that
the library does not print CLI output.

A private inspection-rendering module has one external interface conceptually
equivalent to:

```text
render(LoadedSnapshot, SnapshotSelection, Palette) -> String
```

Its implementation owns private user-facing view types, aggregate counts,
value encoding, cwd compression, tree construction, and styling. The small
interface keeps raw JSON, restore planning, terminal geometry, and internal
pane-recovery variants out of callers.

The data flow is:

```text
latest or explicit path
    -> existing StateStore loader
    -> LoadedSnapshot { path, ValidatedSnapshot }
    -> private InspectView
    -> termtree geometry plus approved palette
    -> stdout
```

The CLI request carries a parsed `SnapshotSelection` and color policy rather
than rediscovering those facts during rendering. Only `LoadedSnapshot` may
enter view construction; raw or merely deserialized snapshot values cannot be
rendered.

## Libraries

Use `termtree` 1.0 for recursive connector geometry and multiline pane labels.
Its custom glyph palette produces the approved `├─`, `│`, and `└─` grammar.
tmux-rescue remains responsible for every node's content and semantics.

Use `anstyle` for the fixed palette and `anstream` for adaptive ANSI output.
The exact `anstyle` and `anstream` versions are already present in the lockfile
through Clap; they become direct dependencies without introducing another
styling stack.

Do not use `ptree`: its global presentation configuration and broader default
feature set would let unrelated environment or configuration state alter this
command. Do not derive a tree directly from snapshot domain types: the approved
view intentionally differs from their implementation vocabulary.

## Loading And Trust

Inspection reuses `StateStore::load_latest` and
`StateStore::load_explicit_path`. This preserves existing guarantees for:

- one-time `latest` selection;
- canonical relative pointer validation;
- opening the selected immutable file beneath `snapshots/`;
- explicit-path symlink rejection;
- regular-file and size enforcement; and
- `RawSnapshot` to `ValidatedSnapshot` refinement.

A missing, dangling, escaping, incoherent, or invalid `latest` is an error.
Inspection never scans historical files for a substitute. Explicit snapshot
paths retain the existing explicit-path contract.

## Errors And Streams

Successful inspection writes the entire document to standard output and
nothing to standard error. Stable and unstable valid snapshots use this same
stream contract.

Selection, environment, file, and validation errors produce no standard output
and use the existing terminal-safe error wording on standard error. When color
is active, only the fatal `error:` prefix is red. Output-write failures exit
`1`; reporting a second error is best effort when the affected stream is no
longer writable.

## Documentation

Implementation updates:

- `README.md` with the default and explicit inspect forms;
- `docs/src/DESIGN.md` with inspection in the v1 user experience; and
- `docs/src/ARCHITECTURE.md` with the command, selection/trust behavior,
  rendering ownership, stream contract, and exit status.

`docs/src/TOOL-RECOVERIES.md` remains the authority for captured tool payloads
and does not need presentation details.

## Verification

CLI parsing tests cover default latest selection, explicit paths, every color
mode, and invalid color values.

Plain exact-output tests cover:

- multiple sessions, windows, and panes;
- every pane fact variant;
- full snapshot timestamps and paths;
- stable and unstable topology;
- unavailable program information followed by the rest of the tree;
- equal and differing session/pane cwd values;
- program aggregation and first-seen order;
- command quoting, empty arguments, controls, literal escapes, Unicode, and
  non-UTF-8 bytes; and
- names and values that resemble ANSI sequences or internal classification
  words.

Styled exact-output tests verify that only approved tokens are colored and that
every style is reset immediately. Stripping ANSI from forced-color output must
produce byte-for-byte identical output to `--color never`.

Integration tests cover latest and explicit loading, invalid snapshots, empty
stdout on failure, exit statuses, and absence of tmux/process/preflight access.
Mapping tests assert that the renderer adds no internal classification labels;
they do not reject matching words that came from snapshot data.

Repository verification runs formatting, Clippy with warnings denied, the full
locked test suite, documentation generation, and Cargo packaging.

## Non-goals

- Raw or pretty JSON output.
- Restore-plan or current-machine preflight information.
- Reading live tmux or process state.
- Filtering, sorting, collapsing, or alternate recovery-first views.
- Terminal-width-dependent layout, truncation, paging, or an interactive TUI.
- ASCII connector mode.
- Reconstructing unstable-capture events that were not persisted.
- Changing the snapshot schema or recovery behavior.
