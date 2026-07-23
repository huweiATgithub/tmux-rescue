# Automatic Recovery Whitelist

## Role

This document is the authority for which foreground programs tmux-rescue may
recover automatically and for the evidence each resolver must require.

The architecture and safety rules for classification, restore planning, and
execution are defined in [ARCHITECTURE.md](ARCHITECTURE.md). A program not
listed here must not be classified as an automatic recovery.

## Document Index

| Section | What it covers |
| --- | --- |
| [Whitelist](#whitelist) | Closed v1 variants, required identity, and restore command |
| [Shared Resolver Contract](#shared-resolver-contract) | Outcomes and evidence rules common to every whitelist entry |
| [Codex](#codex) | Codex recognition, exact session evidence, and resume payload |
| [Claude Code](#claude-code) | Claude Code recognition, exact session evidence, and resume payload |
| [mdBook Serve](#mdbook-serve) | `mdbook serve` recognition and argv replay |
| [Bookshelf Serve](#bookshelf-serve) | `book serve` recognition and argv replay |

## Whitelist

| Program | Recognition | Required recovery identity | Restore command |
| --- | --- | --- | --- |
| Codex | Foreground Codex TUI tied to the pane | Exact Codex session ID | `codex resume <session-id>` |
| Claude Code | Foreground Claude Code TUI tied to the pane | Exact Claude session ID | `claude --resume <session-id>` |
| mdBook | Foreground `mdbook serve` | Captured executable and full argv | Replay captured argv |
| Bookshelf | Foreground `book serve` | Captured executable and full argv | Replay captured argv |

No other program is in the v1 automatic-recovery whitelist.

The persisted automatic-recovery type is exactly:

```text
AutomaticRecovery =
  | Codex { session_id: CodexSessionId }
  | ClaudeCode { session_id: ClaudeSessionId }
  | MdBookServe { command: RecognizedMdBookServeCommand }
  | BookshelfServe { command: RecognizedBookshelfServeCommand }
```

The Codex and Claude variants derive their canonical resume argv from the
validated session ID; no separately serialized command can disagree with that
identity. The serve-command variants wrap a `CapturedCommand` only after their
constructors prove the executable and argv rules in their sections. Raw JSON
cannot construct any of these refined payloads directly.

## Shared Resolver Contract

The generic `PaneTiedForegroundEvidence` input, `ResolverOutcome` type, and its
mapping to pane recovery states are defined in
[ARCHITECTURE.md](ARCHITECTURE.md). This document owns the evidence required to
construct each `AutomaticRecovery` variant.

`Automatic` is permitted only when a variant constructor has the exact identity
or lossless argv required by its whitelist entry. Unknown, incomplete, or
conflicting evidence does not authorize automatic execution.

### Evidence Rules

- Process evidence must belong to the pane's foreground process, not merely to
  a process with a matching name elsewhere on the host.
- Session-oriented tools require one exact session ID. A newest or most recent
  session heuristic is not sufficient.
- A tool's generic server, daemon, or app-server process is not evidence that a
  particular interactive pane owns a session.
- Conflicting exact identifiers are a resolver failure, not a choice between
  candidates.
- Files and process metadata used as evidence are untrusted input and must be
  parsed before use.

### Tail Corroboration

Captured pane tail may corroborate process or tool evidence, but it never
creates or selects a session ID.

- A clear mismatch exists only when the captured tail contains a syntactically
  valid session ID explicitly attributed to that same tool and it differs from
  the resolver's exact ID. The resolver returns `ConflictingEvidence`, which
  prevents automatic recovery.
- Missing or inconclusive tail does not block automatic recovery when the
  resolver otherwise has exact, pane-tied evidence.

### Command And Startup Contract

Automatic recovery either derives canonical structured argv from a refined
session ID or carries a recognized structured serve command. Restore renders
that structure for the target shell and submits it with Enter. After startup,
the executor applies the shared bounded-settle and guarded-input rules from
[ARCHITECTURE.md](ARCHITECTURE.md), using the variant's exact success predicate.

## Codex

### Recognition

The foreground process must be a Codex interactive TUI tied to the pane.
Recognizing a generic Codex app-server process is not sufficient.

### Identity Evidence

The resolver may inspect session files opened by the pane's foreground Codex
process under the Codex session store. An unopened file is not a candidate. For
each candidate, it parses the first JSONL object and requires:

- `type == "session_meta"`;
- `payload.originator == "codex-tui"`;
- `payload.thread_source == "user"`;
- `payload.cwd` exactly matches the pane working-directory evidence;
- `payload.parent_thread_id` is absent or null; and
- `payload.id` contains the exact session ID.

Exactly one candidate must satisfy the requirements. Zero candidates are
insufficient evidence. Multiple candidates, conflicting identifiers, or an
unparseable required field prevent automatic recovery.

`payload.session_id` is metadata and is not the recovery identity. The
resolver must not substitute it for `payload.id`, infer ownership from a
generic app-server process, scan for the newest session, or use `--last`.

### Recovery Payload

```text
AutomaticRecovery::Codex { session_id: CodexSessionId }

derived argv = ["codex", "resume", <session-id>]
```

The executable name shown here is the v1 recovery command contract. Normal
restore preflight still verifies that it is available in the target shell.
After launch, `RecoveredAutomatically` requires a pane-tied Codex session file
whose parsed `payload.id` equals the requested `CodexSessionId`. A foreground
Codex process without exact identity confirmation becomes `NeedsAttention`.

## Claude Code

### Recognition

The foreground process must be a Claude Code interactive TUI tied to the pane.
Background agents and generic Claude Code service processes are not themselves
recoverable foreground sessions.

### Identity Evidence

The resolver requires one exact session ID tied to the foreground Claude Code
process. Accepted sources are:

- a live foreground argv containing an exact `--session-id` or exact resume
  identity; or
- a parsed record refined to this complete evidence shape:

```text
ClaudeWorkerEvidence {
  session_id: ClaudeSessionId,
  working_directory: RecordedAbsolutePath,
  pid: ProcessId,
  process_start_time: ProcessStartTime,
  transport: PtyIdentity | SocketIdentity
}
```

When using a worker record, every field above is mandatory. The working
directory must equal the pane evidence; PID and process start time must identify
the same live foreground worker; and the PTY or socket identity must bind that
worker to the pane. A missing or mismatched field is insufficient or conflicting
evidence. A daemon roster or status record may corroborate these facts but
cannot supply a missing field, and daemon health alone does not identify the
pane's session.

Record shapes may vary between supported Claude Code versions; a display name
is not required. The parser must reject a shape it does not understand rather
than fill missing identity fields heuristically.

Zero candidates are insufficient evidence. Multiple candidates or conflicting
identifiers prevent automatic recovery. The resolver must not select the
newest entry under the Claude project store.

### Recovery Payload

```text
AutomaticRecovery::ClaudeCode { session_id: ClaudeSessionId }

derived argv = ["claude", "--resume", <session-id>]
```

The executable name shown here is the v1 recovery command contract. Normal
restore preflight still verifies that it is available in the target shell.
After launch, `RecoveredAutomatically` requires pane-tied argv or a complete
worker record carrying the requested `ClaudeSessionId`. A foreground Claude
Code process without exact identity confirmation becomes `NeedsAttention`.

## mdBook Serve

### Recognition

The pane-tied foreground evidence must satisfy all of:

- the inspected process executable basename is `mdbook`;
- the captured `argv[0]` basename is `mdbook`; and
- captured `argv[1]` is exactly `serve`.

Name-only matches elsewhere on the host do not qualify. Missing argv elements
or disagreement between the executable and command word do not construct an
automatic variant.

### Recovery Payload

The resolver refines the foreground executable and complete lossless argv into
`RecognizedMdBookServeCommand`, stored as:

```text
AutomaticRecovery::MdBookServe {
  command: RecognizedMdBookServeCommand
}
```

Restore renders its captured argv without inventing, dropping, or prepending
arguments. Post-launch success requires pane-tied foreground evidence that
satisfies the same recognition rules and captured serve argv.

## Bookshelf Serve

### Recognition

The pane-tied foreground evidence must satisfy all of:

- the inspected process executable basename is `book`;
- the captured `argv[0]` basename is `book`; and
- captured `argv[1]` is exactly `serve`.

Name-only matches elsewhere on the host do not qualify. Missing argv elements
or disagreement between the executable and command word do not construct an
automatic variant.

### Recovery Payload

The resolver refines the foreground executable and complete lossless argv into
`RecognizedBookshelfServeCommand`, stored as:

```text
AutomaticRecovery::BookshelfServe {
  command: RecognizedBookshelfServeCommand
}
```

Restore renders its captured argv without inventing, dropping, or prepending
arguments. Post-launch success requires pane-tied foreground evidence that
satisfies the same recognition rules and captured serve argv.
