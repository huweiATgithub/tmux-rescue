# tmux-rescue Architecture

## Role

This document is the implementation contract for tmux-rescue v1. It defines the
stable domain boundaries, data types, workflows, invariants, and observable
outcomes needed before implementation.

It intentionally does not prescribe an internal module tree, exact function
names, or trait granularity. Those details may change as implementation exposes
better boundaries.

The automatic-recovery variants and their tool-specific evidence are owned by
[TOOL-RECOVERIES.md](TOOL-RECOVERIES.md).

## System Boundary

tmux-rescue observes one live source tmux server, stores immutable snapshots,
and may reconstruct a selected snapshot on one selected destination tmux
server. Live-server selection and stored-snapshot selection are separate
boundaries.

The selector and destination types are:

```text
TmuxSelector =
    SocketName(OsString)  // emit as -L <raw-value>
  | SocketPath(OsString)  // emit as -S <raw-value>

RestoreDestination {
  selector: TmuxSelector
}
```

`TmuxSelector` encodes the parser's proof that the two spellings are exclusive.
Its value is a lossless operating-system string. tmux-rescue does not make a
path absolute, canonicalize a value, derive a path from `-L`, inspect tmux's
selector environment rules, prepare a selector-derived directory, or reject a
value based on reimplemented tmux semantics. `RestoreDestination` owns exactly
one selector; `RestorePlan` owns exactly one destination.

One command-construction boundary appends the selector's exact flag and raw
value as two `std::process::Command` arguments. It never reconstructs an
argument from display text. An explicit selector remains unchanged for the
lifetime of its source adapter or restore destination, including relative and
non-UTF-8 values. tmux-reported source paths are provenance; they do not replace
or reinterpret an explicit selector.

Snapshot source and restore destination are separate concepts. The snapshot's
validated `SnapshotSource` records the tmux-reported absolute source path.
Restore uses an explicit selector when supplied; otherwise it deliberately
wraps that recorded path as `TmuxSelector::SocketPath`. This generated fallback
is restore policy, not resolution of a user selector. Neither choice restricts
which immutable snapshot may be restored.

`OwnedRestoreTarget` is returned only after an execution-time claim proves the
attempt token, server PID, tmux server start time, operating-system process
start time, and zero sessions. It is required for topology mutation and normal
rollback. A failed claim returns no owned capability. Cleanup after a possible
start uses a narrower cleanup-only proof of the same token, PID, and start-time
facts; incomplete or changed evidence authorizes no mutation and produces a
conservative observed disposition.

## Rust Boundary

The Cargo package exposes:

- a reusable library crate containing domain types, validation, capture,
  planning, execution, storage behavior, and typed outcomes; and
- a thin CLI binary crate containing argument parsing, human-readable output,
  and exit-status mapping.

External interactions are replaceable capabilities. The library needs
interfaces for tmux, process inspection, snapshot storage, capture time, and
target-shell rendering. Typed capture events and restore outcomes carry
diagnostics across the library boundary. Real adapters and test adapters use
the same contracts.

The library never parses CLI arguments, prints output, or terminates the
process. A future scheduler will call the library and real adapters directly
rather than invoke or parse the CLI.

The binary has one raw-argument boundary. It parses root-level `-L` or `-S`
directly into an optional `TmuxSelector`, then passes that refined value to the
capture adapter or restore planner. Downstream code does not carry parallel
optional selector values or recheck their exclusivity. `inspect` accepts no
selector.

Human snapshot inspection is owned by a private binary module. It accepts only
a storage-produced `LoadedSnapshot`, constructs private display facts, and
renders terminal text. Snapshot domain types therefore do not acquire terminal
geometry, styling, or printing responsibilities.

The CLI parses `IconMode = Unicode | Nerd` as a typed value, defaulting to the
portable Unicode mode. It passes that value to the private inspection renderer.
The renderer's private symbol palette owns the window, pane, and working-
directory glyphs, including the session-working-directory reference marker;
neither glyph choices nor terminal tree layout belong to serialized snapshot
types or the reusable library.

The exact module layout and interface decomposition are implementation
decisions.

## Snapshot Domain Model

The serialized shape is parsed in two stages:

```text
JSON -> RawSnapshot -> ValidatedSnapshot
```

Only `ValidatedSnapshot` may enter restore planning. v1 snapshots
intentionally have no schema-version field.

The conceptual persisted model is:

```text
RawSnapshot {
  captured_at,
  source,
  consistency,
  sessions
}

ValidatedSnapshot {
  captured_at: CaptureTime,
  source: SnapshotSource,
  consistency: CaptureConsistency,
  sessions: NonEmptyUniqueOrdered<SessionName, SessionSnapshot>
}

CaptureConsistency =
  Stable
  | Unstable { attempts: ExhaustedAttemptCount }

SessionSnapshot {
  name: SessionName,
  working_directory: RecordedAbsolutePath,
  windows: NonEmptyUniqueOrdered<WindowIndex, WindowSnapshot>
}

WindowSnapshot {
  source_index: WindowIndex,
  name: WindowName,
  panes: NonEmptyUniqueOrdered<PaneIndex, PaneSnapshot>
}

PaneSnapshot {
  source_index: PaneIndex,
  working_directory: RecordedAbsolutePath,
  recovery: PaneRecovery
}

PaneRecovery =
  Idle
  | Automatic(AutomaticRecovery)
  | Manual(CapturedCommand)
  | Unavailable(CaptureFailure)

CapturedCommand {
  executable: Executable,
  argv: NonEmpty<LosslessArgument>
}
```

`AutomaticRecovery` is a closed enum. Its members and payload contracts are
defined only in [TOOL-RECOVERIES.md](TOOL-RECOVERIES.md).

`CapturedCommand` preserves the executable identity and the complete observed
argv, including `argv[0]`, with argument boundaries intact. `Executable` and
`argv[0]` are nonempty operating-system strings; later argv elements may be
empty because empty arguments are semantically significant. Serialization must
represent non-UTF-8 values losslessly rather than discarding or replacing
bytes.

The executable records the inspected process image and is classification
evidence. Manual and fallback hints render captured `argv[0]` followed by the
remaining argv elements; they do not prepend the captured executable.
Automatic launch instead replaces `argv[0]` with the absolute target executable
proved by preflight and preserves every remaining argument. This binds the
executed command to `PlanningExecutable` even when the target shell's `PATH`
differs from the invoking environment.

`RecordedAbsolutePath` proves only that the captured value is syntactically
absolute. Whether that directory still exists is intentionally resolved later
during restore preflight.

`CaptureFailure` records a bounded, terminal-safe explanation of why
foreground recovery data could not be captured. It is recovery state, not
human-reminder metadata.

`ValidatedSnapshot` and its nested unique collections are opaque outside their
capture and raw-refinement constructors. The invariants below therefore cannot
be bypassed by directly constructing their fields.

### Snapshot Invariants

A validated snapshot guarantees:

- it describes exactly one source tmux server;
- the snapshot has at least one session, every session at least one window, and
  every window at least one pane;
- session names are unique within the snapshot;
- window indexes are unique within their session;
- pane indexes are unique within their window;
- child order is explicit and deterministic;
- every window has a required name;
- every session and pane has its own required recorded working directory;
- every pane has exactly one `PaneRecovery` value;
- manual recovery contains a complete, nonempty argv;
- automatic recovery is a valid member of the closed whitelist;
- an unstable attempt count proves exhaustion of
  `MAX_TOPOLOGY_VALIDATION_ATTEMPTS`; and
- reminder metadata, environment variables, shell identity, and layout data are
  absent.

Window and pane indexes are source coordinates scoped by their parent, not
durable tmux ids. Restore preserves window indexes when possible on the fresh
target. Pane source order is preserved, but exact destination pane indexes are
not a success requirement.

## Capture Contract

v1 capture is manual. The core exposes one capture operation used by:

```text
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] snapshot
```

Without an explicit selector, the first no-start tmux query uses the invoking
tmux context or tmux's default selection. It observes `#{socket_path}`, validates
that value as `SnapshotSource`, and pins all later commands in that capture to a
generated `SocketPath` selector. With an explicit selector, the initial query
and every later source command use the original `-L` or `-S` value unchanged;
the query's reported path is recorded only as source provenance. A missing,
rejected, or malformed selection is an error and publishes nothing.

The source adapter retains both the validated provenance and the selector used
for subsequent commands. Source commands use tmux's no-start mode. Capture does
not intentionally create a selected server, assign local meaning to the
selector, or change selector-relevant working directory or inherited
environment between calls.

Every successful invocation creates a new snapshot. Capture performs no
comparison or deduplication against earlier snapshots.

### Topology Validation

The core policy constant is:

```rust
const MAX_TOPOLOGY_VALIDATION_ATTEMPTS: usize = 3;
```

It is not configurable in v1. The policy name, rather than its current numeric
value, is used throughout the capture workflow.

One attempt is:

```text
read topology_before
-> capture structural and pane recovery data
-> read topology_after
-> compare topology fingerprints
```

The topology fingerprint contains only the canonical ownership tree:

```text
session_name -> window_index -> pane_index
```

Working directories, window names, foreground processes, and recovery payloads
are not part of topology equality.

Every started attempt consumes one unit of the policy limit. Its result is one
of:

```text
StableCandidate(candidate)
CompleteUnverifiedCandidate(candidate, reason)
NoCompleteCandidate(reason)
```

Matching before and after fingerprints produce `StableCandidate`. A mismatch
or a failed after-read following a complete candidate produces
`CompleteUnverifiedCandidate`. A failed before-read or incomplete structural
capture produces `NoCompleteCandidate`. Any result other than a stable
candidate causes a full retry while attempts remain.

When the limit is exhausted, capture publishes the most recent complete
candidate as `CaptureConsistency::Unstable { attempts }` and logs every reason
that prevented validation. If no complete candidate was produced, capture
fails without writing a snapshot. `ExhaustedAttemptCount` accepts only the
current `MAX_TOPOLOGY_VALIDATION_ATTEMPTS` value in v1 snapshots.

A candidate is complete when every topology node has a structurally valid
record. `PaneRecovery::Unavailable` is a valid pane record, so one process
inspection failure does not discard all other recovery data.

### Pane Classification

For each pane, capture attempts to refine process inspection into
`PaneTiedForegroundEvidence`, which binds the foreground process and its
structured command to that pane. Whitelist resolvers accept only successfully
refined evidence and return:

```text
ResolverOutcome =
  | Automatic(AutomaticRecovery)
  | NotRecognized
  | InsufficientEvidence(ResolverFailure)
  | ConflictingEvidence(ResolverFailure)
```

Classification maps that result as follows:

- no foreground program beyond the pane shell becomes `Idle`;
- exactly resolved whitelist evidence becomes `Automatic`;
- `NotRecognized`, insufficient evidence, or conflicting evidence becomes
  `Manual` when the full foreground command is readable;
- unreadable foreground process details become `Unavailable`.

v1 records only that foreground command. It does not capture or reconstruct a
process tree.

Opened tool-session record sets and contents are collected before and after the
final foreground-process re-observation. A collection failure makes that
record source unavailable, and any change across the closing fence invalidates
the observation; stale record metadata cannot authorize automatic recovery.

Automatic resolution uses the complete evidence contract in
[TOOL-RECOVERIES.md](TOOL-RECOVERIES.md).

Capture reports unavailable pane data, resolver downgrades, and topology
validation failures through typed events. The CLI renders those events; the
core does not print.

## Snapshot Storage Contract

When `XDG_STATE_HOME` is set, the default state root is
`$XDG_STATE_HOME/tmux-rescue`. Otherwise it is
`~/.local/state/tmux-rescue`. Its layout is:

```text
tmux-rescue/
|-- snapshots/
|   \-- <capture-timestamp>-<unique-suffix>.json
\-- latest -> snapshots/<capture-timestamp>-<unique-suffix>.json
```

This is one global stream. The layout has no selector, socket-name, or
socket-path partition. Every successful capture publishes into the same
`snapshots/` directory and participates in ordering the same `latest` pointer,
even when successive captures selected different tmux servers.

Each final filename parses into a canonical `SnapshotKey` containing the
capture timestamp and an RFC 4122 version 4 UUID suffix. Filename creation is
no-replace, so concurrent invocations cannot overwrite one another. Timestamp
is the primary recency key; the unique suffix provides a deterministic
tie-breaker for equal timestamps. A detected clock regression saves the
snapshot but does not move `latest` backward and is logged. Valid historical
snapshots are never modified or automatically deleted in v1.

State directories and snapshot files are owner-only by default. Temporary
publication files are not historical snapshots and may be removed after failed
or interrupted publication. Preparation secures and syncs every required state
directory, then syncs every ancestor directory entry through the filesystem
root before snapshot publication begins. This also covers a multi-level path
that a concurrent publisher may have created first.

### Publication

Snapshot files do not require a capture-wide lock:

1. Write a unique temporary file inside `snapshots/`.
2. Flush and sync the complete JSON.
3. Atomically rename it, with no replacement, to its final timestamped key.
4. Sync the `snapshots/` directory entry.
5. Acquire a short operating-system advisory lock for the global `latest`
   update.
6. Compare the new capture key with the current valid target when one exists.
7. Atomically replace the `latest` symlink only when the new snapshot is
   newer.
8. Sync the state-root directory entry and release the lock.

The lock serializes only pointer ordering. Concurrent capture and immutable-file
writes remain allowed, and the operating system releases the lock after a
process crash.

Immutable publication commits after the final rename and successful
`snapshots/` directory sync. Publication returns one of:

```text
SnapshotPublication =
  | NotPublished(PublicationFailure)
  | PublicationIndeterminate { candidate_path, failure }
  | Published {
      snapshot_path,
      consistency,
      latest: Updated | KeptNewer | ReplacedInvalid | UpdateFailed(failure)
    }
```

A failure before the final rename is `NotPublished`. A failure after rename but
before its directory sync is `PublicationIndeterminate`; the reported path may
survive but is never used to update `latest`. Once committed, a later pointer
failure does not undo or modify the immutable snapshot.

Both `Stable` and `Unstable` complete snapshots may become
`latest`. Recency is preferred over the older snapshot's stronger topology
consistency.

A crash before immutable publication may leave only a temporary file. A crash
after immutable publication but before pointer replacement may leave a valid
snapshot not referenced by `latest`. In either case, the previous symlink
remains intact.

A missing, dangling, or invalid `latest` is an inspection or restore selection
error. Neither command silently guesses another file. The user may always
provide an explicit immutable snapshot path. A later successful capture may
atomically replace an invalid pointer and reports `ReplacedInvalid`.

The `latest` symlink is valid only when it is a relative link naming a snapshot
inside this state root's `snapshots/` directory. Its basename must parse as a
canonical `SnapshotKey`, and that key's timestamp must equal the opened
snapshot's validated `captured_at`. Default loading rejects an absolute,
escaping, noncanonical, incoherent, or otherwise unexpected link target before
using it. This key constraint does not apply to an explicitly selected snapshot
path.

Default selection resolves `latest` once, opens the selected target as a
regular file beneath `snapshots/`, and validates and deserializes that same
opened file. It does not validate one path and then follow `latest` again.

## Inspection Contract

The command shape is:

```text
tmux-rescue inspect [SNAPSHOT] [--color <auto|always|never>] [--icons <unicode|nerd>]
```

Without `SNAPSHOT`, inspection uses the same one-time global `latest` selection
and validation contract described above. An explicit path uses the explicit
loader and does not require a state-root environment. The complete data flow is:

```text
latest or explicit path
    -> StateStore validated load
    -> LoadedSnapshot
    -> binary-private InspectView
    -> termtree geometry plus anstyle palette
    -> one stdout document
```

Only `LoadedSnapshot` may enter view construction. Inspection never reads raw
JSON directly, contacts tmux, inspects processes, constructs a `RestorePlan`,
or performs target/resource preflight.

The document contains snapshot identity and capture metadata, aggregate
contents and visible-program counts, and the complete validated
session/window/pane tree in stored order. Pane presentation reports the facts
stored in the snapshot: a tool session and ID, a shell, a captured command and
executable, or unavailable program information and its reason. Working
directories are always present; exact byte equality with the containing
session path is rendered as the session-working-directory reference marker
(`cwd = ◆`).

`termtree` owns recursive connector geometry only. Private display types own
node content, ordered aggregation, command boundaries, and cwd compression.
Lossless operating-system values preserve printable Unicode, visibly escape
controls and literal escapes, and encode invalid UTF-8 bytes as `\xNN` before
any style is added. Commands remain a diagnostic argv representation rather
than a promise of shell-copy execution. No value is truncated or silently
canonicalized for display.

The fixed palette uses standard ANSI colors and the terminal default
foreground. Cyan marks only the session `◆`, green the stable `●`, yellow the
unstable `▲` and unavailable-program `!`, and red the fatal `error:` prefix.
Selected snapshot identity, names, pane facts, and the unstable warning phrase
may be bold. Connectors, paths, IDs, indexes, counts, reasons, and summary
entries remain uncolored. Removing ANSI leaves byte-identical plain text.

`--color auto` uses `anstream` to resolve stdout and stderr support separately,
including its terminal and environment conventions. `always` and `never`
override that automatic result.

A valid stable or unstable snapshot produces one complete stdout document,
writes nothing to stderr, and exits 0. `PaneRecovery::Unavailable` is a valid
fact with the same successful stream contract. Selection, loading, validation,
rendering, or output failure exits 1 and reports a terminal-safe error on
stderr; failures before a document write leave stdout empty. Inspection emits
no progress messages and has no partial-success status.

## Restore Contract

Restore is plan-first:

```text
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] restore [SNAPSHOT]
    # validate, preflight resources, print plan, stop
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] restore [SNAPSHOT] --run
    # print the same plan, then execute
```

The selector is optional, mutually exclusive, non-repeatable, and root-level.
`SNAPSHOT` selects an explicit immutable file; omitting it selects the one
global `latest`. These choices are independent: `-L abc restore` means restore
the global latest snapshot to tmux's `-L abc` destination, not restore the
latest snapshot captured from that server.

An explicit destination selector is retained byte-for-byte. When it is omitted,
planning constructs `TmuxSelector::SocketPath(snapshot.source().path())` from
the selected snapshot's validated source provenance. Rendering and execution
both borrow that single selector from the plan's `RestoreDestination`; neither
can silently fall back to the snapshot source after an explicit selector was
supplied.

Plan-only restore makes no tmux call against the destination and performs no
filesystem writes. It neither derives a destination path nor claims that the
destination is vacant, absent, available, resolved, or claimable. `--run` may
therefore print a valid plan and then fail at the execution-time ownership
claim. A destination that reaches an existing server fails that claim without
topology mutation.

### Validation And Preflight

Planning performs:

```text
snapshot selection
-> deserialize RawSnapshot
-> refine ValidatedSnapshot
-> choose explicit TmuxSelector or generate SocketPath(snapshot.source.path)
-> construct RestoreDestination
-> resolve TargetShell and current resources
-> construct RestorePlan
```

All snapshot paths, names, commands, indexes, collection sizes, and whitelist
payloads are validated before planning. The plan identifies every intended
topology operation, automatic launch, manual hint, fallback, and expected
result.

Planning produces only refined actions:

```text
RestorePlan {
  destination: RestoreDestination,
  target_shell: TargetShell,
  sessions: NonEmptyUniqueOrdered<SessionName, PlannedSession>,
  panes: NonEmptyUniqueOrdered<SourcePaneCoordinate, PlannedPaneAction>,
  degradations: Ordered<PlanDegradation>
}

PlannedPaneAction =
  | LeaveIdle { directory: ResolvedDirectory }
  | LaunchAutomatic {
      directory: ExistingRecordedDirectory,
      input: LaunchableShellInput,
      expected: AutomaticRecoveryExpectation
    }
  | PasteManualHint {
      directory: ResolvedDirectory,
      input: RenderedShellInput
    }
  | PasteAutomaticFallback {
      directory: ResolvedDirectory,
      input: RenderedShellInput,
      reason: AutomaticFallbackReason
    }
  | NoInput {
      directory: ResolvedDirectory,
      reason: CapturedRecoveryUnavailable
    }
```

`ResolvedDirectory` retains whether it is the recorded directory or a named
fallback. `RenderedShellInput` is derived from validated structured argv,
contains no submitted Enter, is bound to the selected `TargetShell`, and is no
larger than `MAX_RENDERED_SHELL_INPUT_BYTES`. The bound keeps the rendered
input and its nested tmux command representation below the operating system's
single-argument limit.
`LaunchableShellInput` additionally proves that the command word resolves to an
available executable in an existing recorded directory. `RestorePlan` and its
actions are opaque products of planning; execution cannot reconstruct an action
from raw snapshot fields. The plan contains no parallel absolute destination
identity and no destination-availability capability.

The first plan line renders the exact stored selector:

```text
target: -L abc
target: -S ./rescue.sock
target: -S /recorded/source.sock  # generated fallback when no selector was given
```

Only one of those lines appears. Arbitrary operating-system strings use the
CLI's safe diagnostic escaping. Display text does not feed execution; the
original `OsString` remains in `TmuxSelector`.

Preflight resolves current working directories and executables where possible:

- a missing session directory falls back to the target user's home directory;
- a missing pane directory falls back to its session's resolved directory;
- missing directories are never created;
- a program is not launched in a fallback directory; its command becomes a
  paste-only hint;
- a missing executable for an intended automatic recovery similarly becomes a
  paste-only hint; and
- an idle pane remains an interactive shell in its resolved directory.

Every directory fallback is a plan degradation and makes `--run` partial. An
executable fallback for an intended automatic recovery is also a degradation.
The default plan-only command still succeeds after printing those degradations.

Every restored pane uses one `TargetShell`. For the server that execution may
claim, v1 follows tmux's new-server default order and selects the first suitable
native Linux executable at a full path from:

1. the invocation's `SHELL` environment variable;
2. the effective user's `getpwuid(3)` shell; or
3. `/bin/sh`.

Script wrappers are not accepted as `TargetShell` in v1 because their runtime
process identity would be the script interpreter rather than the selected
path. Both the selected path's basename and the canonical runtime path's
basename must identify a supported shell dialect; an unrelated native program
behind a shell-named symbolic link is rejected. The canonical runtime must be a
structurally complete architecture-compatible Linux ELF with file-bounded load
segments and an executable entry point, and must match a supported conventional
system-shell path or a supported entry in `/etc/shells`. Planning parses the
runtime and captures its file identity from the same opened file. Construction
does not execute a candidate shell, so plan-only restore has no shell-startup
side effects.

Topology creation passes this shell explicitly for every pane and sets it as
the owned server's `default-shell`; a tmux configuration override cannot make
the printed plan and executed shell diverge. The same typed value governs
rendering, pane creation, and the input guard. Automatic executable lookup uses
the invocation environment's `PATH`, resolves one absolute executable, and
stores that executable's file identity in the plan; launch does not depend on a
later shell lookup. Source shell identity is not captured. The plan also records
the shell executable's file identity and topology creation rechecks it before
starting panes. If the shell cannot be resolved or a command cannot be rendered
safely for it, planning fails.

### Execution Phases

Execution has a strict rollback boundary.

#### Phase 1: Topology

The executor first passes the plan's `RestoreDestination` to the target adapter.
The adapter creates an unpredictable token in a private claim configuration and
runs its sole start-capable destination command:

```text
tmux <exact-selector> -f <claim-config> start-server
```

Every other destination client—claim confirmation and recheck, topology,
recovery, verification, failed-claim cleanup, and rollback—uses tmux no-start
mode (`-N`) with the same selector. If the endpoint disappears, these clients
fail closed instead of starting a replacement server.

The start command alone is not ownership. Through the same selector, claim
confirmation reads the attempt token, server PID, tmux server start time, and
session count; captures the operating-system process start time for that PID;
repeats the tmux observation to reject an identity change; and proves the same
process is still live. Only a matching token, stable PID and both start-time
facts, and zero sessions produce `OwnedRestoreTarget`. The capability retains
the destination and ownership facts required by every later mutation. An
existing server does not acquire the new token from the claim configuration, so
a selector that reaches one produces no owned capability and leaves its
process, sessions, topology, and options untouched.

After successful ownership proof, the executor:

- creates sessions with their resolved working directories;
- creates windows at their recorded indexes and restores their names;
- creates panes in source order using a deterministic default layout;
- starts every pane with the planned `TargetShell` as the target server's
  default interactive shell; and
- starts each pane in its resolved working directory.

To establish a session's working directory when its first pane has a different
working directory, topology creation starts a non-interactive blocking
placeholder through `TargetShell`, then replaces it with that pane's
interactive shell. The first interactive shell therefore starts exactly once,
in the pane's resolved working directory.

Exact split positions, sizes, and pane indexes are not restored.

Every mutation is scoped by `OwnedRestoreTarget`. If topology creation fails,
rollback consumes that ownership value and returns a verified target
disposition:

```text
TargetDisposition = Removed | Retained | Missing | Unknown

RestoreTargetState =
  | NotEstablished
  | Observed(TargetDisposition)
```

Normal topology rollback returns `Removed`. Cleanup failure or an indeterminate
endpoint is fatal, prominently logged, and reported with `Retained` or
`Unknown`; it is never summarized as successful removal. The executor cannot
remove a target for which it lacks `OwnedRestoreTarget`. For an owned server,
an absent endpoint yields `Removed` only after its recorded Linux process is
also proved gone or replaced; the same live PID/start identity yields
`Retained`.

A claim failure before `start-server` may have begun reports
`RestoreTargetState::NotEstablished`. Once the command may have run, failure
returns no `OwnedRestoreTarget` and reports an evidence-based
`RestoreTargetState::Observed(Removed | Retained | Missing | Unknown)`.
Failed-claim cleanup is authorized by a separate cleanup-only capability, not
by an inference from command success or endpoint absence. That capability is
created only when the same selector still observes this attempt's matching
token, PID, tmux server start time, operating-system process start time, and
zero sessions; it rechecks those facts before a guarded `kill-server`. Missing,
mismatched, or changed evidence leaves the server untouched and the disposition
conservative.

#### Phase 2: Program Recovery

Once program recovery begins, the target server is never rolled back.
Independent panes continue after local failures.

Execution switches on `PlannedPaneAction`, not the snapshot's `PaneRecovery`.
Before any action sends input, it invokes one guarded pane operation that binds
the owned target, recorded pane PID, planned shell identity, rendered input, and
whether the typed operation is paste-only or an automatic launch. Automatic
launch also carries and rechecks the planning-time executable file identity.
The adapter performs a full Linux foreground-process re-observation immediately
before a tmux-side conditional check of server ownership, pane liveness, pane
PID, and current-command basename. The Linux observation also requires the
planned pane working directory. Here `Idle` means that the expected interactive
shell is the pane foreground process; it does not claim that a visible prompt
has been recognized. The adapter refuses input when either check detects a
missing pane or a non-shell foreground process. It does not return a reusable
shell-verification token.

An unexpected foreground process becomes `NeedsAttention` and receives no
input; a missing pane is a local failure. This gate applies to automatic
commands, planned manual hints, preflight fallback hints, and
failed-automatic hints.

- `LeaveIdle`: verify and leave the interactive shell untouched.
- `LaunchAutomatic`: bracketed-paste the rendered recovery input literally,
  then submit one separate Enter.
- `PasteManualHint`: paste the rendered foreground command without Enter.
- `PasteAutomaticFallback`: paste the rendered recovery command without Enter.
- `NoInput`: send no input and report the capture failure.

After an automatic launch receives the bounded automatic-settle policy, the
executor observes the pane using the whitelist variant's recovery expectation:

- the expected whitelist variant and exact recovery identity, or exact
  recognized replay command, means recovery succeeded;
- a verified idle shell means launch failed or exited, so the recovery command
  is pasted without Enter through a new guarded pane operation;
- another foreground process becomes `NeedsAttention` and receives no input;
  and
- a missing pane or observation failure is a local failure.

This is a best-effort v1 input boundary, not an atomic Linux process lock: tmux
cannot include foreground PID and process-start identity in the same predicate
that sends pane input. A foreground transition that occurs in the scheduling
gap and is indistinguishable by pane PID and command basename can evade the
second check. The planned shell or automatic executable can likewise be
replaced after its last file-identity check and before tmux receives the input.
v1 keeps these gaps small, never reuses an earlier capture-time observation,
and logs any detectable refusal; eliminating the residual races requires a
stronger future tmux or process-control capability.

### Restore Outcomes

Per-pane execution outcomes are:

```text
RestoredIdleShell
RecoveredAutomatically
PreparedManualHint
PreparedAutomaticFallbackHint(AutomaticFallbackReason)
AutomaticLaunchFailedHintPrepared
NeedsAttention(AttentionReason)
```

`NeedsAttention` covers any pane for which no safe input action can complete,
including unavailable captured recovery data, a blocked process gate, and a
missing target pane. Planned manual hints are normal completion. An unavailable
pane, failed automatic recovery, automatic recovery downgraded during
preflight, any plan degradation, unexpected foreground process, or missing
pane makes the overall execution partial. Overall execution also records the
observed `TargetDisposition`; it never infers retained or removed state from
the attempted operation alone.

Snapshot CLI outcomes are:

```text
0  immutable snapshot published and latest updated, replaced, or kept newer
1  no committed publication, indeterminate publication, or latest update failed
```

Both stable and unstable publication can exit 0; unstable capture is logged as
a warning. A published result prints its immutable path, consistency, and
`latest` disposition to standard output. An indeterminate result prints the
candidate path with an explicit warning. Capture progress and failures go to
standard error.

Inspection CLI outcomes are:

```text
0  validated snapshot document printed, including unstable or unavailable facts
1  selection, loading, validation, rendering, or output failed
```

Stable and unstable inspection share the same success status and stream
contract. The unstable warning is inside the stdout document and the complete
tree follows it. Valid inspection writes nothing to stderr. A fatal error is
reported on stderr; loading and validation failures produce no stdout
document.

Restore CLI outcomes are:

```text
0  plan produced, or execution completed as planned
1  validation, preflight, target-claim, topology, or cleanup failure
2  program recovery began but completed partially
```

Command-line syntax errors occur before a restore request exists; they exit 1
with Clap usage diagnostics and no `RestoreTargetState`. For a parsed restore
request exiting 1 or 2, the final summary reports `RestoreTargetState`.
Validation, preflight, and target-claim failures that never begin target
creation use `NotEstablished`; claim failures after creation begins report an
observed disposition. Normal topology rollback uses `Observed(Removed)`, and
cleanup failure may use `Observed(Retained)` or `Observed(Unknown)`. The restore
plan and final summary go to standard output. Progress, warnings, fatal
diagnostics, and per-pane failures go to standard error. v1 has no JSON-output
contract; future automation calls the typed library API.

## Trust And Safety

Every snapshot is untrusted input, including the global `latest` target and
explicit paths. Successful JSON deserialization proves only raw shape.
Refinement must also enforce:

- required fields and bounded collection and field sizes;
- scoped uniqueness and ordering invariants;
- valid lossless argv encoding;
- valid tmux names, indexes, and source identity;
- closed-whitelist payload invariants; and
- terminal-safe diagnostic data.

The executor accepts only a constructed `RestorePlan`, never raw or merely
deserialized snapshot values.

The inspection renderer accepts only `LoadedSnapshot`, never raw or merely
deserialized values. Lossless values are converted to terminal-safe display
encodings before renderer-owned ANSI sequences are introduced.

Structured argv is rendered through the validated target-shell adapter.
Recovery text is sent literally, and Enter is a separate operation. Manual and
fallback hints cannot contain embedded Enter or uncontrolled terminal
sequences. Logs escape unsafe control data.

Restore never mutates an existing target server, never creates a missing
working directory, and never removes a server it cannot prove it created.

## Error Visibility

No error is silently converted into success.

Capture, inspection loading, planning, storage, topology execution, and pane
recovery return typed outcomes. Capture events identify the attempt and source
pane when applicable; planning, storage, and execution results retain their
failure or fallback, and each pane result identifies its source coordinate.
The CLI renders them and prints a final per-pane restore inventory.

Expected plan-only completion and planned manual recovery are successful.
Fatal pre-execution failures and partial post-topology recovery remain
distinguishable through exit status.

## Verification Boundaries

Core tests use fake external capabilities to verify:

- raw-to-validated snapshot refinement;
- invalid-state and malicious-input rejection;
- exclusive `TmuxSelector` variants and exact two-argument `-L`/`-S` emission,
  including empty, relative, and non-UTF-8 values;
- lossless argv round-tripping, including empty arguments and non-UTF-8 values,
  and target-shell rendering;
- stable capture, topology retries, and unstable publication at
  `MAX_TOPOLOGY_VALIDATION_ATTEMPTS`;
- before-read, after-read, and incomplete-candidate attempt outcomes;
- ambient source pinning to the reported path and explicit-selector retention on
  every source command;
- idle, automatic, manual, and unavailable pane classification;
- rejection of incoherent or cross-variant automatic payloads;
- missing-directory and missing-executable planned actions and degradation
  provenance;
- deterministic `TargetShell` selection across planning and execution;
- explicit destination independence, snapshot-source fallback, and exact
  selector rendering from `RestorePlan`;
- no destination call or filesystem write during plan-only restore;
- exactly one start-capable claim command and no-start mode for confirmation,
  topology, recovery, verification, failed-claim cleanup, and rollback;
- existing-server protection, full token/PID/tmux-start/process-start/session
  ownership proof, cleanup-only proof, ownership-scoped topology, and rollback
  cleanup failure;
- best-effort pane continuation after recovery begins;
- a foreground-process change during guarded input, with no input sent;
- exact automatic identity or serve-command confirmation after launch;
- hint pasting without Enter only through the guarded input operation; and
- typed outcome and exit-status mapping.

Binary renderer and CLI tests verify root-level selector grammar and conflicts,
exact selector plan output, the generated source-path fallback, and plan-before-
execution ordering. They also verify exact plain and styled inspection output,
lossless command/path display encoding, ordered program aggregation, latest and
explicit snapshot selection, unstable and unavailable continuation, per-stream
color policy, empty stdout on fatal loading failures, and the absence of live
tmux, process, restore-planning, or preflight access during inspection.

Storage tests inject failure before and after each publication commit boundary
and cover no-replace immutable creation, equal-timestamp collisions, detected
clock regression, multi-level concurrent directory durability, canonical key
and snapshot-time coherence, global symlink replacement, concurrent `latest`
ordering, replacement of `latest` between selection and file opening, and
captures from different selected servers sharing the same archive and pointer.

Whitelist resolver tests use tool metadata fixtures and cover every documented
downgrade condition, canonical command derivation, and exact post-launch
success predicate.

Real-tmux integration tests create uniquely addressed temporary `-L` and `-S`
servers and temporary state directories. They exercise exact selector
pass-through for snapshot and restore, global-latest selection, source-path
fallback, and protection of an existing server's process, sessions, topology,
and options after claim-token mismatch. They run serially and never inspect,
mutate, or target the user's default tmux server.
