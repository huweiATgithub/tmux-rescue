# tmux Server Selection Design

## Goal

Let one tmux-rescue invocation select the tmux server used by `snapshot` or
`restore` with the familiar tmux `-L` and `-S` spellings. A selector identifies
only a live source server or a restore destination. It never selects a snapshot
stream.

tmux-rescue keeps one global immutable snapshot archive and one global `latest`
pointer. Captures from different tmux servers share that stream. Restore selects
a snapshot independently from the destination server on which it reconstructs
the snapshot.

This feature mirrors tmux's option placement and socket-name/path model. It is
not a CLI compatibility promise: tmux-rescue deliberately accepts at most one
selector, accepts each selector at most once, and keeps plan-only restore free
of filesystem mutation.

## Command Surface

The command grammar is:

```text
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] snapshot
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] restore [SNAPSHOT] [--run]
```

`-L` and `-S` are root options and must precede the subcommand. Only the short
spellings are provided.

An invocation may contain neither selector or exactly one occurrence of one
selector. The argument parser rejects:

```text
tmux-rescue -L a -S /tmp/b restore
tmux-rescue -L a -L b restore
tmux-rescue -S /tmp/a -S /tmp/b restore
tmux-rescue snapshot -L a
```

Neither selector overrides the other, and repeated values do not use
last-value-wins behavior.

## Selector Meaning

`-L SOCKET_NAME` uses tmux's named-socket model. The selected endpoint is
formed beneath the tmux per-user socket directory:

```text
<socket-base>/tmux-<real-uid>/<socket-name>
```

Socket-base candidates are considered in tmux order: `TMUX_TMPDIR` when it is
set, then `/tmp`. Resolution selects the first candidate whose real path can be
resolved, without creating the base or per-user directory. An empty, missing,
or otherwise unresolvable `TMUX_TMPDIR` therefore falls back to `/tmp`. A
candidate such as `/dev/null` whose real path resolves but is not a usable
directory remains selected and fails later directory or target checks rather
than silently changing endpoints.

The socket name is a lossless operating-system string, not a sanitized filename
or an application-level server identifier. Resolution appends its raw bytes in
the same role they have for tmux. It does not reject or normalize embedded path
separators, `.` segments, or `..` segments. In particular, an absolute-looking
socket name is still appended beneath the named-socket prefix rather than
replacing that prefix. An empty socket name is accepted and resolves to the
per-user directory path itself; later target checks determine that this is not
a usable socket endpoint.

`-S SOCKET_PATH` uses tmux's alternative socket-path model. An absolute path is
retained. A relative path is made absolute against the invocation's current
working directory exactly once. Resolution does not require the target to
exist, follow symlinks, or canonicalize lexical path segments. An empty value
is accepted as a zero-component relative path and therefore resolves to the
invocation's current working directory; later target checks reject that
directory as a socket endpoint.

Both forms resolve to one absolute `TmuxServerIdentity`. All tmux operations
after resolution use `tmux -S <resolved-path>` with inherited `TMUX` selection
removed. This pins the operation to one endpoint and prevents later working
directory or environment changes from changing its meaning.

Resolution fails before capture or restore planning when the selected value,
current directory, user identity, or named-socket base cannot produce one
deterministic absolute endpoint. Diagnostics preserve lossless path rendering
and remain terminal-safe.

## Refined Selector Types

The CLI initially receives at most one raw operating-system value. It
immediately parses that value into an exclusive selector type conceptually
equivalent to:

```text
ServerSelector =
  | NamedSocket(TmuxSocketName)
  | SocketPath(TmuxSocketPath)
```

There is no downstream pair of optional `-L` and `-S` values. The argument
parser carries the mutual-exclusion and single-occurrence guarantees; the
selector type carries which interpretation applies.

Resolution returns a value conceptually equivalent to:

```text
ResolvedServerSelector =
  | NamedSocket {
      identity: TmuxServerIdentity,
      preparation: NamedSocketPreparation {
        socket_directory: TmuxSocketDirectory,
        owner: RealUserId
      }
    }
  | SocketPath {
      identity: TmuxServerIdentity
    }
```

The named variant retains the per-user directory and real UID derived during
resolution. Those facts are needed only if restore execution must prepare the
directory. They are not discarded and later recomputed or inferred again from
the absolute socket path. The `NamedSocketPreparation` type's fixed contract
owns the `mode & 0o007 == 0` access rule; it is not a caller-configurable value.

The exact Rust names are implementation details. The required invariants are:

- downstream code sees one selected form, never conflicting raw options;
- every selected form carries one resolved absolute server identity;
- named-socket provenance and preparation requirements remain available to
  restore execution; and
- target probing, rendering, creation, mutation, and rollback all consume the
  same identity.

## One Global Snapshot Stream

Server selection does not change storage layout. The existing state root keeps:

```text
tmux-rescue/
|-- snapshots/
|   \-- <capture-timestamp>-<unique-suffix>.json
\-- latest -> snapshots/<capture-timestamp>-<unique-suffix>.json
```

Every successful capture publishes a new immutable file into the same
`snapshots/` directory. Existing timestamp and unique-suffix ordering governs
the one global `latest` update. A newer capture from any selected server may
advance the pointer; it does not overwrite or modify an older immutable file.

The source server remains recorded inside each snapshot. It is provenance and
the default restore destination, not a partition key. tmux-rescue does not add
per-server directories, per-server `latest` pointers, selector-derived state
roots, or collision rules based on server identity.

## Snapshot Workflow

Without a selector, `tmux-rescue snapshot` preserves the current behavior: it
asks tmux which server is selected by the invoking tmux context.

With `-L` or `-S`, snapshot resolves the selector and contacts that exact live
server. It does not start a server and does not create a named-socket directory.
Failure to contact the selected server produces no snapshot candidate.

After contact, snapshot obtains the server-reported socket path, refines it to
an absolute `SnapshotSource`, and pins every topology and pane observation to
that identity with `tmux -S`. The server-reported identity, rather than the raw
selector spelling, is persisted in the snapshot.

Capture and publication otherwise retain their existing contracts. In
particular, publication always targets the global stream:

```text
optional selector
    -> resolved server endpoint
    -> live server-reported SnapshotSource
    -> capture and validate candidate
    -> global immutable snapshots/
    -> global latest update
```

## Restore Workflow

Snapshot selection and destination selection are independent:

- `SNAPSHOT` selects an explicit immutable snapshot;
- omitting `SNAPSHOT` selects the global `latest` snapshot;
- `-L` or `-S` selects the restore destination; and
- omitting a server selector uses the selected snapshot's recorded source as
  the destination.

Consequently, `tmux-rescue -L abc restore` means "restore the global latest
snapshot to named server `abc`". It does not mean "restore the latest snapshot
captured from `abc`".

The restore data flow is:

```text
raw CLI
    -> exclusive refined selector, when present
    -> resolved absolute selector, without mutation
snapshot argument or global latest
    -> loaded ValidatedSnapshot
resolved selector identity or snapshot source
    -> one resolved restore destination
    -> target vacancy preflight
    -> RestorePlan
```

The resolved restore destination carries both the absolute identity used by the
existing restore safety model and any named-socket preparation provenance.
It is conceptually:

```text
ResolvedRestoreDestination {
  identity: TmuxServerIdentity,
  preparation: None | NamedSocketPreparation
}
```

Planning refines that value as:

```text
AvailableRestoreDestination {
  destination: ResolvedRestoreDestination,
  vacancy: TargetVacancy
}
```

`RestorePlan` owns exactly one such availability-refined destination. It does
not store a second parallel target identity. The renderer and executor borrow
the identity from that one value, and execution consumes the plan rather than
resolving the selector again.

### Plan Output

`restore` without `--run` prints the plan for the destination selected by this
data flow. Its `target:` line is the resolved absolute socket path that was
actually preflighted:

```text
tmux-rescue -L abc restore

target: /tmp/tmux-1000/abc
target vacancy: missing path
...
```

The concrete base and UID reflect the invocation environment. For `-S
./rescue.sock`, the target line contains the path made absolute against the
invocation's current working directory. With no selector, it contains the
snapshot's recorded source path.

The plan renderer reads the destination from `RestorePlan`; it does not read
the snapshot source independently. A selected destination therefore cannot be
lost or replaced by the source during rendering.

`restore --run` prints that same plan before execution. The absolute identity
shown in the plan is also the identity rechecked, claimed, mutated, and, when
required, rolled back. Execution does not recompute `TMUX_TMPDIR`, the real UID,
or the invocation working directory.

## Plan-Only And Execution Mutation

Selector resolution and plan-only restore are read-only. In particular,
resolving `-L` does not create `<socket-base>/tmux-<real-uid>` merely to print a
plan. The missing destination may still be preflighted and displayed as a
missing path.

For a destination selected with `-L`, `restore --run` prepares the resolved
per-user directory immediately before the execution-time vacancy recheck and
target claim:

1. If the per-user directory is missing, create that directory with mode
   `0700`.
2. If it exists, require an actual directory, not a symlink, owned by the
   already-resolved real UID and satisfying `mode & 0o007 == 0`. Group-class
   permission bits do not violate this tmux-compatible check.
3. Do not create the socket name's additional parent segments or the
   `TMUX_TMPDIR` base.
4. Recheck vacancy for the exact planned `TmuxServerIdentity`.
5. Claim that endpoint and continue through the existing ownership proof.

The restore executor owns this ordering. Its target capability exposes a
preparation operation that consumes the plan-owned preparation requirements;
the real adapter performs the filesystem work and test adapters observe the
same interface. Preparation is not performed opportunistically by the CLI or
inside claim. A preparation failure produces a fatal target-preparation
failure with the target not established. It cannot reach the execution-time
vacancy recheck, server claim, or topology mutation.

Destinations selected with `-S`, and destinations defaulted from snapshot
source, receive no named-socket directory preparation. tmux-rescue does not
infer `-L` provenance from an arbitrary stored absolute path.

All existing target protections remain in force: a live server, non-socket
path, inaccessible endpoint, or indeterminate state fails closed; a missing
path or refused stale socket is only a vacancy observation; execution rechecks
vacancy; and only a proven-owned server may be mutated or rolled back.

## Errors And Observable Behavior

Argument-shape errors, including mixed or repeated selectors, are reported by
the CLI parser before command dispatch.

Selector-resolution errors occur before tmux access, snapshot capture, or
restore planning. Snapshot connection and capture failures keep the existing
no-publication behavior.

Restore planning probes the exact selected destination, and successful plan
rendering prints it. An existing or indeterminate destination prevents a plan
from being established, including in plan-only mode. `--run` may still fail
after printing a valid plan if named directory preparation or the
execution-time vacancy recheck fails. Such a failure occurs before topology
mutation and uses the existing fatal restore result shape.

The selector does not change snapshot schema, restore exit statuses, progress
streams, or pane recovery outcomes.

## Documentation

Implementation synchronizes the public documentation with this design:

- `README.md` shows selector placement for snapshot and restore and explains
  the global stream;
- `docs/src/DESIGN.md` describes source selection, destination selection, and
  plan-first behavior; and
- `docs/src/ARCHITECTURE.md` defines the refined selector, resolution,
  preparation, global storage, and plan/run identity contracts.

`docs/src/TOOL-RECOVERIES.md` remains solely about program recovery and does
not need selector details. Historical implementation plans remain historical.

## Verification

CLI parsing tests cover:

- no selector, one `-L`, and one `-S` before each supported subcommand;
- rejection when `-L` and `-S` appear together;
- rejection when either selector is repeated; and
- rejection when a selector appears after the subcommand.

There is deliberately no test whose purpose is to reject `--target`; that
option is simply removed. Tests specify the supported current command surface,
not compatibility behavior for a deleted spelling.

Selector-resolution tests cover:

- named resolution with `TMUX_TMPDIR` unset, resolvable, empty, missing, and
  resolved-but-unsuitable;
- real-UID directory construction;
- raw named-socket values, including empty values, path separators, and lexical
  segments;
- absolute, relative, and empty `-S` values;
- lossless non-UTF-8 operating-system values; and
- absence of filesystem mutation during resolution.

Snapshot tests cover:

- unchanged invoking-context selection when no selector is present;
- explicit `-L` and `-S` selection of isolated live servers;
- persistence of the selected server's reported absolute socket identity;
- failure without server or socket-directory creation when selection cannot
  connect; and
- interleaved captures from different servers publishing immutable files into
  one archive and updating one global `latest` pointer.

Restore planning tests cover:

- source-path defaulting with no selector;
- `-L` and `-S` overriding only the destination;
- explicit snapshot and global-latest selection remaining independent of that
  destination;
- the human plan's `target:` line containing the exact resolved destination;
  and
- plan-only named selection creating no directories or sockets.

Restore execution tests cover named-directory creation and validation, the
exact real-UID and `0o007` mode rule, preparation failure before recheck and
claim, execution-time vacancy recheck after preparation, and use of the same
single plan-owned absolute identity by rendering, execution-time recheck,
claim, topology, recovery, and rollback. Existing isolated-socket tests
continue to prove that an already live or indeterminate target is never
mutated.

Repository verification runs formatting, Clippy with warnings denied, the full
locked test suite, documentation generation, and Cargo packaging.

## Non-Goals

- Per-server snapshot directories, histories, or `latest` pointers.
- Treating a server selector as a snapshot selector or filter.
- Long selector aliases, compatibility aliases, or deprecation behavior.
- More than one selector or last-value-wins parsing.
- Server creation during snapshot or filesystem mutation during plan-only
  restore.
- Creating arbitrary parent directories for `-S` paths or nested `-L` names.
- Changing the snapshot schema, capture payload, pane recovery policy, or
  fresh-target ownership model.
- Adding server selectors to snapshot inspection or future read-only snapshot
  commands.
