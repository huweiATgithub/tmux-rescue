# tmux Server Selection Design

## Goal

Add tmux-style server selection to `snapshot` and `restore` without copying
tmux's selector semantics into tmux-rescue.

The command-line selector is an opaque instruction for tmux. tmux-rescue
preserves the selected flag and its operating-system string value, then emits
that pair on the tmux commands that act on the selected server. tmux remains
the sole authority for interpreting socket names, socket paths, environment,
working-directory effects, and errors.

Server selection does not partition stored snapshots. All captures continue
to publish into one global snapshot stream, and restore destination selection
remains independent of snapshot selection.

## Command Surface

The supported forms are:

```text
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] snapshot
tmux-rescue [-L SOCKET_NAME | -S SOCKET_PATH] restore [SNAPSHOT] [--run]
```

The selector belongs to the root command and therefore appears before the
subcommand. There are no long-form aliases.

The parser enforces only the selector's command shape:

- `-L` and `-S` are mutually exclusive;
- either flag may appear at most once; and
- a selector after the subcommand is rejected.

There is no precedence rule and no last-value-wins rule. `--target` is removed
rather than retained as an alias.

## Opaque Selector Contract

The parsed value is represented as one exclusive type:

```text
TmuxSelector =
    SocketName(OsString)  // emit as: -L <value>
  | SocketPath(OsString)  // emit as: -S <value>
```

This type encodes the parser's proof that exactly one spelling was selected.
Downstream code does not carry separate optional `-L` and `-S` values and does
not recheck their exclusivity.

The value remains a lossless operating-system string. tmux-rescue does not:

- make a `-S` value absolute;
- canonicalize either value;
- derive a socket path from `-L`;
- inspect `TMUX_TMPDIR`, user IDs, or tmux's default socket directory;
- create or validate a selector-derived directory; or
- reject a selector based on tmux's presumed naming or path rules.

One command-construction boundary owns selector emission. Given a
`TmuxSelector`, it appends exactly the corresponding flag and raw value as two
arguments to `std::process::Command`. It does not render a shell command or
reconstruct the value from display text.

The original selector is retained for the lifetime of the source or restore
target. Every tmux invocation against that endpoint receives the same flag and
raw value. tmux-rescue does not change the invocation working directory or
selector-relevant inherited environment between those commands, so an opaque
relative value is not silently redirected. A tmux-reported socket path may be
observed and recorded, but it does not replace or reinterpret an explicit
selector.

## One Global Snapshot Stream

Server selection changes which live server is captured, not where its
snapshot is stored.

The existing state layout remains one global stream:

```text
<state-root>/
  snapshots/
    <snapshot-id>.json
  latest
```

There is no selector, socket name, or socket path component in this layout.
Every successful `snapshot` invocation publishes an immutable snapshot to the
same `snapshots/` directory and participates in updating the same `latest`
pointer under the existing ordering and publication rules.

Consequently, snapshots from different tmux servers may advance the same
global `latest` pointer. An explicit immutable snapshot path remains the way
to restore a capture other than the global latest capture.

## Snapshot Workflow

Snapshot source selection is:

- no selector: use tmux's normal ambient server selection; or
- `-L` or `-S`: pass that exact selector to tmux.

For an explicit selector, every tmux command used to identify and capture the
source receives the original selector. tmux-rescue does not first turn `-L`
into `-S`, and it does not normalize an explicit `-S` value.

For ambient selection, the initial tmux query observes the selected server's
`#{socket_path}` and uses that observed path as `-S` for subsequent commands in
the same capture. This preserves the existing single-source capture behavior
without assigning meaning to a user-supplied selector.

Source discovery retains tmux's no-start mode. Snapshot does not intentionally
create a server when the selection is absent; any selector-related filesystem
behavior before tmux reports failure remains tmux's behavior.

The source metadata query records tmux's reported absolute socket path in the
snapshot. That path is source provenance and supports the existing default
restore destination. It is not presented as tmux-rescue's resolution of the
selector. If the reported value does not satisfy the snapshot schema, capture
fails before publication as it does for other invalid source metadata.

The workflow is:

```text
raw root arguments
    -> optional TmuxSelector
    -> tmux source commands with the same explicit selector
    -> tmux-reported source metadata
    -> validated immutable snapshot
    -> global publication
```

A failed selection, connection, metadata query, or capture publishes nothing.

## Restore Workflow

Snapshot selection and destination selection are independent:

- `SNAPSHOT` selects an explicit immutable snapshot;
- omitting `SNAPSHOT` selects the global `latest` snapshot;
- `-L` or `-S` selects the restore destination; and
- omitting a selector uses the selected snapshot's recorded source path as
  `-S <recorded-path>`.

Thus `tmux-rescue -L abc restore` means "restore the global latest snapshot to
the server tmux selects for `-L abc`". It does not mean "restore the latest
snapshot captured from that server".

The restore data flow is:

```text
raw root arguments
    -> optional exclusive TmuxSelector
snapshot argument or global latest
    -> ValidatedSnapshot
explicit selector or SocketPath(snapshot.source.path)
    -> RestoreDestination
    -> RestorePlan
```

`RestoreDestination` owns exactly one `TmuxSelector`. `RestorePlan` owns that
destination and has no parallel absolute target identity or plan-time vacancy
capability. Rendering and execution both borrow the destination from the plan,
so neither can silently fall back to the snapshot source when an explicit
selector was supplied.

The snapshot's recorded source is already a validated absolute path. Wrapping
it as `SocketPath` when no selector was supplied is an explicit restore policy,
not an attempt to resolve a user selector.

## Plan Output

`restore` always renders the destination selector stored in `RestorePlan`.
Examples include:

```text
tmux-rescue -L abc restore

target: -L abc
...
```

```text
tmux-rescue -S ./rescue.sock restore

target: -S ./rescue.sock
...
```

With no explicit selector, the line shows the generated `-S` selector for the
snapshot source:

```text
target: -S /recorded/source.sock
...
```

The argument is rendered with the CLI's existing safe escaping for arbitrary
operating-system strings. Display escaping is diagnostic only; execution uses
the original `OsString`, never the rendered text.

The plan does not label the target vacant, absent, resolved, or available.
Plan-only restore does not ask tmux to connect to or create the destination, so
successful plan rendering is not a claim that execution can establish it.

`restore --run` prints the same plan before attempting execution. The executor
then consumes the selector already stored in that plan.

## Plan-Only And Execution

Without `--run`, restore loads and validates the snapshot, computes recovery
actions, and prints the plan. It makes no tmux command against the destination
and performs no filesystem writes.

With `--run`, the restore adapter passes the planned selector directly to the
tmux command that attempts to start and claim a fresh server:

```text
tmux <exact-selector> -f <claim-config> start-server
```

tmux decides what the selector means and whether the operation succeeds.
tmux-rescue does not preflight a derived socket path, prepare a directory, or
infer that a refused or missing path is available.

This claim command is the only target command allowed to start a server. Every
confirmation, ownership recheck, topology, recovery, verification, cleanup,
and rollback client command uses tmux's no-start mode with the same selector.
If the endpoint disappears, those commands fail closed instead of starting a
replacement server.

Starting a server is not sufficient proof of ownership. The existing
fail-closed claim protocol is retained and generalized to an opaque selector:

1. Generate an unpredictable ownership token in the claim configuration.
2. Invoke tmux with the exact planned selector and that configuration.
3. Through the same selector, read back the token, server PID, tmux-reported
   server start time, and session count, then obtain that PID's
   operating-system process start time.
4. Establish an owned target only when the token matches, both start-time
   observations and the PID are available, and the server has no sessions.

If the selector reaches an existing server, tmux does not establish the new
claim token. A missing or mismatched token therefore leaves the destination
unowned. Cleanup must not kill or mutate that server.

After a successful claim, the owned-target capability retains both the exact
selector and the established token, PID, tmux server start time, and
operating-system process start time. Every topology, recovery, verification,
and rollback tmux command receives that same selector. Mutating commands
remain guarded by those ownership facts. They do not derive or compare an
expected socket path from the selector.

If claim confirmation fails after tmux may have created a server, cleanup is
allowed only when the same selector still reaches the server carrying this
attempt's token, PID, tmux server start time, and operating-system process start
time. Otherwise the final target disposition is reported conservatively and no
unproven server is killed.

A claim failure never returns an `OwnedRestoreTarget` capability. A failure
before `start-server` can have begun reports `RestoreTargetState::NotEstablished`.
Once `start-server` may have run, failure cleanup reports an evidence-based
`RestoreTargetState::Observed(Removed | Retained | Missing | Unknown)` even
though ownership was not established for continued execution. Topology failure
and rollback retain the existing terminal result model. Selector pass-through
does not weaken the rule that only a proven-owned server may be mutated or
rolled back.

## Errors And Observable Behavior

Mixed selectors, repeated selectors, missing selector arguments, and selector
placement after the subcommand are parser errors before command dispatch.

There is no tmux-rescue selector-resolution error category. Once parsed, a
selector's validity and meaning are tmux's responsibility. A tmux rejection,
connection failure, or command failure is reported in the surrounding
snapshot or restore operation without publishing a snapshot or claiming a
successful restore.

Plan-only restore can succeed even when the destination already exists or
cannot later be created, because it deliberately makes no target call.
`restore --run` may therefore print a valid plan and then fail during the
claim. Such a failure occurs before topology mutation and uses the existing
fatal restore result shape.

The selector does not change snapshot schema, snapshot publication semantics,
restore exit statuses, progress streams, or pane recovery outcomes.

## Documentation

Implementation synchronizes the public documentation with this design:

- `README.md` shows root-level selector placement for snapshot and restore and
  explains the single global snapshot stream;
- `docs/src/DESIGN.md` describes source selection, destination selection,
  plan-only behavior, and execution-time claim; and
- `docs/src/ARCHITECTURE.md` defines the opaque selector type, command-boundary
  pass-through contract, global storage, and owned-target safety contract.

`docs/src/TOOL-RECOVERIES.md` remains about program recovery and does not need
selector details. Historical implementation plans remain historical.

## Verification

CLI parsing tests cover:

- no selector, one `-L`, and one `-S` before each supported subcommand;
- rejection when `-L` and `-S` appear together;
- rejection when either selector is repeated; and
- rejection when a selector appears after the subcommand.

There is deliberately no test whose purpose is to reject `--target`; that
option is simply removed. Tests describe the supported current command
surface, not compatibility behavior for a deleted spelling.

Command-construction tests cover:

- exact `-L` and `-S` flag/value argument pairs;
- preservation of empty, path-looking, and non-UTF-8 values without assigning
  them local semantics;
- the original explicit selector on every source tmux command;
- the planned selector on claim, topology, recovery, verification, and
  rollback commands;
- no-start mode on every target client command except the initial claim; and
- `-S <snapshot-source>` when restore has no explicit selector.

Snapshot tests cover:

- ambient capture and explicit `-L` and `-S` capture;
- tmux-reported source-path provenance;
- no selected server being started on source-discovery failure;
- no publication after a selector or source-query failure; and
- captures from different servers sharing one snapshots directory and one
  `latest` pointer.

Restore planning tests cover:

- explicit selector independence from snapshot selection;
- fallback to the selected snapshot's source path;
- exact selector display in plan output;
- no destination tmux command in plan-only mode; and
- no filesystem write anywhere in plan-only mode.

Restore execution tests cover:

- claim through explicit `-L`, explicit `-S`, and snapshot-source fallback;
- an existing server remaining untouched after token mismatch;
- pre-start claim failure reporting `NotEstablished`;
- post-start-attempt claim failure returning no owned capability and reporting
  an observed final disposition;
- all post-claim commands retaining the plan's exact selector;
- ownership rechecks preventing mutation after endpoint replacement; and
- conditional cleanup and rollback using token, PID, tmux server start time,
  and operating-system process-start-time evidence.

Real-tmux integration tests exercise isolated `-L` and `-S` servers to verify
end-to-end argument pass-through. For both selector forms, a restore attempt
against an existing server proves that its process, sessions, and existing
options remain untouched when the claim token is not established. These tests
assert tmux-rescue's protection boundary and tmux's actual claim-config
behavior, not a reimplementation of tmux's name, path, environment, or
directory rules.

There are no tmux-rescue tests for `TMUX_TMPDIR` resolution, UID-derived
directories, relative-path absolutization, selector canonicalization, or
directory permission rules because none of those behaviors belongs to this
tool.

## Non-Goals

This change does not:

- resolve, normalize, canonicalize, or otherwise interpret `-L` or `-S`;
- provide plan-time destination availability or path-vacancy guarantees;
- introduce per-server snapshot namespaces or per-server `latest` pointers;
- retain `--target` or add long-form selector aliases;
- add selectors to commands other than `snapshot` and `restore`;
- change snapshot format or source provenance fields; or
- change recovery policy for individual programs.
