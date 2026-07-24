# tmux-rescue Design

## Role

This document is the overview and entrypoint for the tmux-rescue design set. It
defines the problem, the v1 user experience, the overall recovery approach, and
the scope boundary.

Implementation contracts and domain types belong in
[ARCHITECTURE.md](ARCHITECTURE.md). The automatic-recovery whitelist and its
tool-specific evidence belong in
[TOOL-RECOVERIES.md](TOOL-RECOVERIES.md).

## Problem

A tmux server can hold the working shape of a machine: which projects are
active, which windows and panes belong to them, and which programs are still
running. When the server disappears after a WSL or host crash, the underlying
Codex and Claude histories may still exist, but their placement and exact
recovery commands are lost.

tmux-rescue records enough durable state to reconstruct that workspace on a
fresh tmux server. Its primary goal is to restore the programs that were
running. It does not try to preserve processes or reproduce all tmux runtime
state.

## v1 User Experience

### Snapshot

`tmux-rescue snapshot` captures one selected tmux server and, when capture and
publication succeed, writes a new independent snapshot. v1 does not compare it
with earlier captures. A failed invocation that cannot produce a complete
candidate writes no historical snapshot. A publication interruption may leave
a temporary file or an unreferenced immutable snapshot as defined by the
storage contract.

The snapshot contains the source server identity, capture metadata, and the
ordered session, window, and pane tree. It also contains the typed recovery
state for each pane. The newly published snapshot may update the global
`latest` symlink.

Snapshot capture is manual in v1. Scheduling, tmux hooks, and an internal daemon
are deferred until the core capture API has proved useful.

### Inspect

`tmux-rescue inspect` validates and displays the global `latest` snapshot by
default. An explicit immutable snapshot path may be supplied instead:

```text
tmux-rescue inspect [SNAPSHOT] [--color <auto|always|never>] [--icons <unicode|nerd>]
```

Inspection is a topology-first view of what the snapshot contains. It shows
capture metadata and consistency, aggregate session/window/pane and program
counts, then the complete ordered session, window, and pane tree. Pane entries
describe captured facts such as Codex or Claude Code sessions, shells, captured
commands, and unavailable program information.

An unstable topology is a prominent inline warning, not a reason to hide the
snapshot. Inspection still displays the complete tree and succeeds. A pane
whose foreground program could not be captured similarly shows its stored
reason and does not suppress later panes.

Inspection is read-only. It does not contact tmux, inspect current processes,
construct a restore plan, or preflight the current machine. Values remain
complete and color is a redundant semantic aid rather than the only carrier of
meaning.

The portable default icon mode uses Unicode index markers; the optional Nerd
mode uses Nerd Font Mono glyphs. A window containing exactly one pane is
compacted to a single `window › pane` line. When a pane's captured working
directory equals its session's captured working directory, the pane shows a
reference to the session heading (`cwd = ◆`) instead of repeating the path; a
different pane directory remains explicit. Windows with multiple panes retain
their full branch structure.

### Restore

`tmux-rescue restore` reads the global `latest` snapshot by default. An
explicit immutable snapshot path may be supplied instead.

Restore is plan-first:

```text
tmux-rescue restore [SNAPSHOT] [--target <server>]
    # validate, preflight, and print the plan

tmux-rescue restore [SNAPSHOT] [--target <server>] --run
    # print the same plan, then execute it
```

Both forms reject a target tmux server that already exists. The target defaults
to the source socket recorded by the snapshot, but the user may select a
different, absent target server.

Execution first creates sessions, windows, panes, and interactive shells. It
then restores programs inside those shells. On topology failure it rolls back
only a server proven to have been created by the current restore and reports
any cleanup failure. Once program recovery begins, the server is retained and
independent panes are recovered on a best-effort basis.

## Recovery Policy

Automatic recovery uses the closed whitelist defined by
[TOOL-RECOVERIES.md](TOOL-RECOVERIES.md). The overview does not repeat the
whitelist so additions and corrections have one documentation authority.

For a pane outside that whitelist, tmux-rescue records the foreground
executable and its complete structured argv. Restore pastes the safely rendered
command into the pane without pressing Enter.

If foreground process information cannot be captured, the pane remains in the
snapshot as unavailable recovery data. Restore still recreates its shell and
working directory, logs the failure, and sends no input.

## What v1 Preserves

For each source tmux server, v1 preserves:

- capture time and topology-consistency result;
- ordered sessions and session names;
- each session working directory;
- ordered windows, source window indexes, and window names;
- ordered panes, source pane indexes, and pane working directories; and
- one typed recovery state per pane.

Window names are structural state and are restored. Session and pane working
directories are independent; each restored pane explicitly uses its own
directory.

## Safety Model

Snapshot files are treated as untrusted serialized input. Inspection and
restore refine a raw snapshot into validated domain types before presentation
or executable planning.

Inspection accepts only a validated loaded snapshot and escapes lossless
operating-system values before adding terminal styles. It performs no live
system access or mutation.

Restore never mutates an existing tmux server, never creates missing working
directories, and never automatically executes a manual recovery command.
Commands are stored as structured argv and rendered for the target interactive
shell only after validation.

Failures are visible in progress logs and the final per-pane summary. A partial
program recovery retains the new target server so the user can inspect it and
finish recovery manually.

## Design Set

- [DESIGN.md](DESIGN.md): purpose, v1 scope, overall approach, and non-goals.
- [ARCHITECTURE.md](ARCHITECTURE.md): Rust boundaries, domain types, capture and
  restore contracts, storage, safety, outcomes, and verification.
- [TOOL-RECOVERIES.md](TOOL-RECOVERIES.md): authoritative automatic-recovery
  whitelist, recognition evidence, payloads, commands, and downgrade rules.

When the documents overlap, the more specialized document owns the detail.
The overview describes intent; it does not redefine leaf contracts.

## v1 Non-Goals

v1 does not:

- capture automatically, install a scheduler, run a daemon, or install tmux
  hooks;
- restore into, merge with, or replace an existing tmux server;
- restore exact pane positions, split directions, sizes, or layout strings;
- restore active windows, active panes, attached clients, runtime pane or
  window ids, zoom state, or other tmux runtime state;
- capture or restore environment variables;
- persist pane titles, prompts, activity timestamps, transcript tails, or other
  reminder metadata;
- capture or reconstruct process trees beyond the pane's foreground command;
- restore scrollback, editor state, process memory, or unsaved terminal input;
- automatically run commands outside the whitelist;
- provide JSON inspection output, filtering, sorting, collapsing, paging, an
  interactive TUI, ASCII connectors, or terminal-width-dependent truncation;
- automatically delete historical snapshots; or
- version or migrate snapshot schemas.

Those capabilities may be considered later only when they preserve the
program-recovery focus and fit the reusable core-library boundary.
