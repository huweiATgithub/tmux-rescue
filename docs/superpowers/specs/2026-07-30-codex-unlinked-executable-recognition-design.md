# Codex Unlinked Executable Recognition Design

## Role

This design corrects Codex foreground-process recognition when a package
upgrade unlinks the native executable of an already running Codex TUI. It
changes only live Linux process observation and Codex classification.

The existing exact session identity, foreground-process fencing, snapshot
schema, inspection model, restore planning, and paste-without-Enter contracts
remain authoritative and unchanged.

## Goal

Keep an already running Codex TUI eligible for automatic recovery after its
native executable has been unlinked by an upgrade. Recognition must retain the
same process, argv, pane, and session proofs used for a linked executable.

Restore continues to derive `codex resume <session-id>` and resolve the Codex
executable available at restore time. It does not retain, reopen, or execute
the removed image.

The visible success condition is that a qualifying pane is stored as
`AutomaticRecovery::Codex` and therefore inspected as `Codex`, not as the
manual Node launcher command.

## Live Evidence

The reported snapshot was captured at `2026-07-30T02:13:51Z`, immediately
after Bun replaced the global Codex package. A snapshot from before the
replacement contained eleven automatic Codex panes. The reported snapshot
contained zero automatic Codex panes and twelve manual Node commands.

For an affected pane, live process evidence showed:

- a Node launcher and native Codex child in the same foreground process group
  and on the same pane TTY;
- native argv beginning with the original absolute `.../bin/codex` path;
- exactly one opened root Codex session record matching the pane cwd; and
- `/proc/<pid>/exe` rendering the native image as
  `.../bin/codex (deleted)`.

The affected executable inode reported `st_nlink == 0`. A Codex process
started after the upgrade resolved as `.../bin/codex` and reported a positive
link count. The existing classifier requires both the executable basename and
`argv[0]` basename to be exactly `codex`; the kernel decoration therefore made
the otherwise valid native child invisible to Codex resolution.

Inspection did not cause the misclassification. It faithfully rendered the
already persisted `PaneRecovery::Manual` value and used the manual command's
Node executable as its program identity.

## Decision

Refine the executable observation at the Linux procfs seam before recovery
classification. The production value keeps the raw captured command and a
typed executable identity derived from one stably observed executable inode.

Conceptually:

```rust
struct ObservedProcessCommand {
    raw: CapturedCommand,
    executable: ObservedExecutable,
}

struct ProcessExecutableKey {
    device: u64,
    inode: u64,
}

enum ObservedExecutable {
    UnpinnedRaw,
    PinnedLinked {
        key: ProcessExecutableKey,
        link_count: NonZeroU64,
    },
    PinnedUnlinked {
        identity_path: LosslessOsString,
        key: ProcessExecutableKey,
    },
}
```

`ProcessExecutableKey` is deliberately distinct from restore-time executable
identity, which has different proof obligations. Together, the key and enum
fields encode the exact `(st_dev, st_ino, st_nlink)` tuple:
`PinnedLinked` carries a nonzero link count, while `PinnedUnlinked` encodes a
zero link count structurally. Equality also detects changes between positive
link counts.

The raw executable link exists only in `CapturedCommand`; the executable state
does not duplicate it. The concrete representation remains crate-private. The
constructors that produce `PinnedLinked` or `PinnedUnlinked` are private to
Linux process observation and accept only a pinned acquisition result.
Existing public foreground-evidence constructors that accept a
`CapturedCommand` bridge it into `UnpinnedRaw`, whose identity is the complete
raw executable value. Raw text supplied through those APIs can therefore keep
the existing exact-path behavior but can never manufacture unlinked proof. No
public constructor accepts a link count, deletion flag, or replacement
identity path. `LinuxProcessInspector` instead uses crate-private
foreground-evidence constructors that accept an already refined
`ObservedProcessCommand`.

Foreground evidence owns the observed command internally. Recovery code
receives only the operations it needs: the untouched raw command and the
refined executable identity basename. Existing public command accessors still
return `CapturedCommand`; they do not expose a detached boolean that callers
could apply to a different path. Only the Codex predicate consumes the refined
identity basename. Idle-shell, Claude, serve-tool, diagnostic, and manual
fallback behavior continues to consume the raw `CapturedCommand`.

This is a live observation type, not serialized state. `CapturedCommand`
remains the snapshot command type, and the snapshot schema does not gain an
executable-link-state field.

### Refinement Rules

The observed executable is `PinnedLinked` when the pinned executable inode has
a positive link count. Its complete raw link is also its identity path. A
linked file literally named `codex (deleted)` therefore retains that complete
basename and does not classify as Codex. `UnpinnedRaw` follows the same
identity rule but carries no inode claim.

The observed executable is `PinnedUnlinked` only when all of these facts hold:

- the executable inode is pinned through `/proc/<pid>/exe`;
- stable metadata for that inode reports `st_nlink == 0`;
- the pinned executable link ends in exactly the ASCII bytes ` (deleted)`;
  and
- removing that one terminal decoration leaves a nonempty value satisfying
  the `LosslessOsString` invariants.

`identity_path` is the raw link with exactly that final decoration removed.
It need not be revalidated as absolute, canonical, or currently present on the
filesystem. Each transient observed command retains its raw link in
`CapturedCommand` for diagnostics. When classification downgrades to manual,
existing behavior still persists the untouched foreground leader command; it
does not replace that command with a candidate member's command. In the
reported Node/native shape, the fallback therefore remains the Node launcher.

An inode with a positive link count and a ` (deleted)` suffix remains linked.
An inode with zero links but no exact terminal decoration is invalid evidence.
If the original dentry is removed while another hard link keeps
`st_nlink > 0`, the process remains unrecognized by this extension. This rare
case is deliberately fail-closed because the string decoration alone cannot
distinguish it from a literal filename.

## Stable Procfs Acquisition

`LinuxProcessInspector` is the only production adapter. No new trait or
public port is introduced for one adapter.

The inspector opens `/proc/<pid>/exe` with Linux `O_PATH | O_CLOEXEC`. When
procfs authorization permits dereference, this pins the executable without
requiring read permission on the executable object, including after unlink.
Procfs authorization failures remain ordinary process-inspection I/O failures.
The inspector obtains `fstat` metadata before and after reading the pinned
descriptor's link through `/proc/self/fd/<fd>`. The device, inode, and link
count must remain identical across those reads.

One process sample acquires that pinned executable observation, reads argv and
cwd, then acquires `/proc/<pid>/exe` again and requires the second pinned
observation to have the same device, inode, link count, and raw link. The
resulting executable proof and command then participate in the existing
two-pass process-observation equality check. Any detected difference in the
executable, link count, raw link, argv, cwd, process identity, or job
relationship produces the existing raced/invalid process failure.

Opening and pinning are necessary because reading `/proc/<pid>/exe`, its
metadata, and `cmdline` as unrelated paths could combine evidence across an
`exec` or package replacement. Process start time alone is insufficient:
Linux preserves a PID's start time across `exec`. This is bounded sampling,
not an atomic kernel snapshot: it detects persistent or sampled changes but
does not claim to detect an adversarial ABA sequence that restores every
observed value between reads.

## Codex Classification

The foreground process group continues to be searched as one rooted tree. A
Node launcher is not sufficient evidence. At least one leader or member must
satisfy the Codex TUI predicate.

For each candidate command, the predicate requires:

- the refined executable identity basename is exactly `codex`;
- raw `argv[0]` has basename exactly `codex`; and
- no argument selects `app-server`, `exec`, or `mcp-server` mode.

An unlinked native child can therefore enter the existing Codex resolver, but
the extension grants no automatic recovery by itself. Each considered
`.jsonl` session file must be inside the configured Codex session store and be
held by a foreground PID that independently satisfies the Codex TUI predicate.
After deduplicating opened files by device and inode, the resolver parses each
file's first JSONL object and requires:

- `type == "session_meta"`;
- `payload.originator == "codex-tui"`;
- `payload.thread_source == "user"`;
- `payload.cwd` exactly matches the pane cwd;
- `payload.parent_thread_id` is absent or null; and
- `payload.id` is a valid UUID.

Exactly one distinct opened-file candidate may satisfy all of those rules.
Two distinct matching files remain conflicting even when they contain the
same `payload.id`.

Missing, malformed, duplicate, or conflicting session evidence retains the
existing manual downgrade. Same-tool tail conflict handling remains
unchanged.

The wrapper's `argv[1]`, script name, package-manager path, and environment do
not authorize Codex recovery. This design does not add Node, Bun, npm, pnpm,
or install-layout recognition.

## Snapshot And Restore Contract

A successfully classified pane stores the existing
`AutomaticRecovery::Codex { session_id, prompt_area }` value. Optional visible
prompt capture continues only after exact automatic Codex classification.

No persisted field records the source executable path, inode, link count, or
deleted state for automatic Codex recovery. Those are capture-time proofs.

Restore continues to derive this canonical argv:

```text
codex resume <session-id>
```

Planning resolves and validates the current target Codex executable under the
existing rules. The old unlinked executable is never a restore candidate.
Inspection remains a pure projection of the validated snapshot and performs
no live reclassification.

## Failure Behavior

Failures while acquiring, refining, or stabilizing the procfs executable
observation prevent construction of foreground evidence. They return the
existing typed process-inspection failure and are captured as
`PaneRecovery::Unavailable`. This applies when:

- `/proc/<pid>/exe` cannot be opened or its pinned link cannot be read;
- executable metadata changes around link acquisition;
- the raw executable link, argv, or process observation changes between the
  existing stability passes;
- zero-link metadata lacks the exact terminal ` (deleted)` decoration;
- removing the decoration would not produce a nonempty `LosslessOsString`.

When process observation succeeds but Codex classification does not, the
existing classifier preserves the untouched foreground `CapturedCommand` as
`PaneRecovery::Manual`. This applies when:

- only the Node wrapper resembles Codex;
- the native command selects a noninteractive Codex mode; or
- exact root-session evidence is absent, malformed, duplicate, or conflicting.

No failure diagnostic includes session contents, prompt text, environment
values, or arbitrary file contents.

## Alternatives Rejected

### Classifier-Local Suffix Stripping

Accepting an executable basename of either `codex` or `codex (deleted)` would
be a smaller edit, but it would trust string shape without encoding the inode
proof. It would also treat a linked executable literally named
`codex (deleted)` as the former `codex` program.

### Node Wrapper Recognition

Recognizing `node .../codex` or a `codex.js` script would survive package
replacement without inspecting the native child, but it would weaken the
existing executable-plus-argv contract and make an arbitrary Node script a
plausible tool identity. The native Codex image and exact opened-session
evidence remain mandatory.

### Persisting The Old Executable

Recording or reopening the unlinked image would couple recovery to obsolete
code and package-manager internals. Restore needs only the exact session ID and
must use the currently installed, independently validated Codex executable.

## Tests

The first RED regression must exercise the existing production inspector and
public `classify_pane` result. A fake-proc native member's `exe` link points to
`/proc/self/fd/<held-fd>` for a temporary file named `codex` that has been
opened and then unlinked. Before implementation, direct `read_link` observes
the held-FD path and the pane produces the Node manual fallback. After the
inspector follows and pins that target with `O_PATH`, it observes the zero-link
`codex (deleted)` identity and the same pane becomes automatic. The fixture
also contains:

- a Node foreground-group leader;
- a native member whose pinned raw executable link ends in
  `codex (deleted)`;
- zero-link inode proof and refined identity basename `codex`;
- native `argv[0]` ending in `codex`; and
- one exact root session file held by that native member.

It must fail before implementation by producing the Node manual fallback and
pass afterward as `AutomaticRecovery::Codex`. The derived restore command must
remain `codex resume <session-id>`.

Focused executable-observation tests must also prove:

- a linked executable named `codex` keeps its identity;
- public raw foreground evidence ending in `codex (deleted)` uses its complete
  raw identity and remains unrecognized;
- a zero-link `codex (deleted)` observation refines to former identity
  `codex`;
- a linked executable literally named `codex (deleted)` is not stripped;
- an unlinked executable literally named `codex (deleted)` loses only the
  kernel's final decoration and still does not identify as `codex`;
- proved unlinked candidates selecting each of `app-server`, `exec`, and
  `mcp-server` remain manual;
- zero links without the exact decoration are rejected;
- metadata or link changes are rejected as a raced observation; and
- every observed command retains its byte-exact raw value while manual
  fallback continues to persist the byte-exact foreground leader command.

Pure refinement and synthetic inconsistency cases stay inside module-private
unit tests. A separate Linux-only, kernel-backed module test must launch a
temporary executable, unlink it while it remains running, and prove that the
production `O_PATH` acquisition helper obtains the zero-link raw and refined
identities. Because `O_PATH` follows the executable link, other fake-proc
fixtures that currently use dangling `exe` symlink targets must use existing
targets; this is fixture plumbing, not a relaxation of assertions or a new
public procfs port.

Existing anti-spoof, foreground-tree, opened-session, capture, inspection,
planning, and restore assertions remain semantically unchanged and green.
Test fixtures may change only where necessary to carry the new live executable
proof. Implementation verification must run the focused recovery/process
tests, the complete serial all-target/all-feature suite, formatting, strict
Clippy, and `git diff --check`.

## Non-Goals

This change does not:

- classify Codex from a Node wrapper alone;
- relax exact session identity or cwd matching;
- scan for a newest session or trust a session ID only because it appears in
  launcher argv;
- add support for other unlinked whitelist tools;
- change snapshot JSON, inspect output rules, restore argv, or target
  executable validation;
- preserve or execute the removed Codex image; or
- add package-manager- or Codex-version-specific branches.

## Acceptance

The design is satisfied when an affected live Node/native Codex pair with a
stably proven unlinked native image and one exact root session is captured as
automatic Codex recovery, while every existing fail-closed identity gate and
serialized contract remains intact.
