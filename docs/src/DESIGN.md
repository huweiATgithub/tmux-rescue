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

Snapshot and restore accept tmux's two server selectors at the root command:

```text
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] snapshot
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] restore [SNAPSHOT] [--run]
```

`-L` and `-S` are mutually exclusive, non-repeatable, and must appear before
the subcommand. tmux-rescue preserves the chosen flag and its lossless
operating-system string as an opaque instruction. It does not resolve a socket
name, normalize a socket path, or reproduce tmux's environment and directory
rules; tmux remains responsible for interpreting the selector.

### Snapshot

Without a selector, `tmux-rescue snapshot` initially uses tmux's ambient server
selection. It observes that server's reported socket path and pins the remaining
capture commands to the generated `-S` selector so one capture cannot drift
between sources. With `-L` or `-S`, every source command retains the exact
explicit selector instead; a reported source path is recorded as provenance but
does not replace or reinterpret it. Source commands use tmux's no-start mode.

When capture and publication succeed, snapshot writes a new independent
snapshot. v1 does not compare it with earlier captures. A failed selection,
source query, or capture that cannot produce a complete candidate writes no
historical snapshot. A publication interruption may leave a temporary file or
an unreferenced immutable snapshot as defined by the storage contract.

The snapshot contains the source server identity, capture metadata, and the
ordered session, window, and pane tree. It also contains the typed recovery
state for each pane. Every selected server publishes into the same
`snapshots/` directory and competes to update the same global `latest` symlink.
Selector values do not partition storage; restoring a non-latest capture uses
its explicit immutable snapshot path.

For a pane already classified as one exact Codex session, the same explicit
snapshot invocation makes a best-effort capture of pending input from the
currently visible supported composer. The capture is optional enrichment: an
empty composer records nothing, and a changing, hidden, scrolled-out, unsafe,
oversized, or unsupported screen retains the exact Codex session recovery
without prompt text. It never scans scrollback or a Codex transcript to fill in
missing input.

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

When a snapshot contains visible Codex pending input, inspection reports only
its visible-row and byte counts. The text is never copied into the inspection
view or terminal output.

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
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] restore [SNAPSHOT]
    # validate, plan, and print without contacting the destination

tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] restore [SNAPSHOT] --run
    # print the same plan, then execute it
```

Snapshot selection and destination selection are independent. `SNAPSHOT`
selects an immutable capture, or its omission selects the global `latest`;
`-L` or `-S` independently selects the destination. With no explicit selector,
restore generates `-S <recorded-source-path>` from the selected snapshot's
validated source provenance.

The printed plan begins with the selector exactly as retained for execution,
using safe diagnostic escaping for arbitrary operating-system strings. It does
not call tmux against the destination, perform application filesystem writes,
or claim that the destination is absent, available, resolved, or claimable.
Consequently a plan may print successfully even when `--run` will later fail to
establish the destination.

Execution passes the printed selector unchanged to its one start-capable
command, which attempts to claim a fresh tmux server before any topology
operation. Every later destination client uses no-start mode and the same
selector. If the selector reaches an existing server, the claim produces no
owned capability and that server is not mutated or removed.

After a successful claim, execution creates sessions, windows, panes, and
interactive shells, then restores programs inside those shells. On topology
failure it rolls back only a proven-owned server and reports any cleanup
failure. If claim confirmation fails after the start attempt, cleanup is
allowed only with complete revalidated evidence. The exact ownership and
cleanup evidence contracts are defined in
[ARCHITECTURE.md](ARCHITECTURE.md). Once program recovery begins, the server is
retained and independent panes are recovered on a best-effort basis.

For a planned Codex recovery with captured pending input, execution first
resumes and confirms the exact session normally. It then makes a fresh exact-
session observation immediately before a guarded literal bracketed paste. The
prompt is prepared without Enter. A changed session, missing pane, ownership
loss, or paste failure sends no prompt input where it can be detected, retains
the recovered server, and reports that pane as needing attention.

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
- one typed recovery state per pane, including optional supported visible Codex
  pending input for an exact Codex session.

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
directories, and never automatically executes a manual recovery command. Only
a fully proven owned destination can reach topology mutation or rollback;
cleanup after an unconfirmed claim has its own narrower, cleanup-only proof.
Commands are stored as structured argv and rendered for the target interactive
shell only after validation.

Captured Codex pending input is coupled to its exact session identity. Human
inspection, restore plans, warnings, and results expose only counts or fixed
status text, never the draft. Restore rechecks the exact session and pane before
one no-Enter paste attempt; it does not submit, retry, or redirect the draft to
a fallback command.

Snapshot JSON, including captured pending input, is plaintext. State files are
owner-only by default, but copied snapshots and backups remain sensitive and
must be protected by the user.

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
- persist pane titles, shell prompts, activity timestamps, transcript tails, or
  other reminder metadata;
- capture or reconstruct process trees beyond the pane's foreground command;
- restore scrollback, editor state, process memory, or hidden, scrolled-out, or
  unsupported unsaved terminal input;
- claim that a visible Codex suffix is the complete draft, recover input from an
  unsupported renderer, or submit recovered pending input;
- automatically run commands outside the whitelist;
- provide JSON inspection output, filtering, sorting, collapsing, paging, an
  interactive TUI, ASCII connectors, or terminal-width-dependent truncation;
- automatically delete historical snapshots; or
- version or migrate snapshot schemas.

Those capabilities may be considered later only when they preserve the
program-recovery focus and fit the reusable core-library boundary.
