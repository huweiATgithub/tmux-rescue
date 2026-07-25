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
| mdBook | Foreground `mdbook serve` | Captured executable and full argv | Launch proven target executable with captured trailing argv |
| Bookshelf | Foreground `book serve` | Captured executable and full argv | Launch proven target executable with captured trailing argv |

No other program is in the v1 automatic-recovery whitelist.

The persisted automatic-recovery type is exactly:

```text
AutomaticRecovery =
  | Codex {
      session_id: CodexSessionId,
      prompt_area: Option<CapturedCodexPromptArea>
    }
  | ClaudeCode { session_id: ClaudeSessionId }
  | MdBookServe { command: RecognizedMdBookServeCommand }
  | BookshelfServe { command: RecognizedBookshelfServeCommand }
```

The Codex and Claude variants derive their canonical resume argv from the
validated session ID; no separately serialized command can disagree with that
identity. The optional Codex prompt area is capture enrichment bound to that
same variant, not command or identity evidence. The serve-command variants wrap
a `CapturedCommand` only after their constructors prove the executable and argv
rules in their sections. Raw JSON cannot construct any of these refined
payloads directly.

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
- Opened tool-session record sets and contents are observed on both sides of
  the final foreground-process fence. A failed record collection cannot supply
  evidence, and a changed observation invalidates the pane observation.

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
that structure for the target shell, bracketed-pastes it literally, and submits
one separate Enter. After startup, the executor applies the shared bounded-settle
and guarded-input rules from [ARCHITECTURE.md](ARCHITECTURE.md), using the
variant's exact success predicate.

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

### Visible Prompt Evidence

After exact session resolution, an explicit snapshot may optionally read the
current visible tmux grid. The parser is intentionally tied to the observed
Codex `0.145.0` renderer and fails closed on other layouts. Its primary frozen
fixture is a `132x40` pane with cursor `(9,37)` and this exact seven-row bottom
suffix:

```text
» The test prompt for recovering.

  Line 1.

  Line 2.

  gpt-5.6-sol ultra · ~/projects/tmux-rescue · main · Context 78% used · 258K window · Fast on · Approve for me · 2.55M used · Main…
```

Transcript rows above that suffix are not prompt input. The accepted composer
rules are:

- the pane is not in copy mode, the cursor leaves at least one empty row before
  the one-line footer, and every row in that inset is empty;
- the first prompt row begins with exactly `› ` or `» `; later nonempty rows
  begin with exactly two ASCII spaces;
- the cursor is at the rendered end of the last nonempty prompt row, or at the
  two-cell textarea start on an empty trailing continuation;
- the footer is either the configured one-line form above with an exact
  `Context N% used` segment, a default ASCII `0..100% context left` form with
  the supported shortcut/queue/plan hints, or the supported narrow collapsed
  hint; and
- the exact empty-composer text `Ask Codex to do anything` at the textarea
  start means no captured prompt.

The renderer-owned glyph and first space are removed from the first row; exactly
two ASCII margin spaces are removed from nonempty continuation rows. Blank rows,
visible soft-wrap boundaries, additional indentation, and a trailing empty
prompt row are preserved. Visible text such as
`[Pasted Content 12345 chars]` is stored literally; tmux-rescue does not resolve
or reconstruct the hidden pasted content behind that placeholder.

This is a visible-suffix contract, not a completeness claim. A draft beginning
above the current grid may yield only its visible suffix. Hidden, scrolled-out,
popup-covered, copy-mode, unsafe, oversized, changing, or otherwise unsupported
input is omitted. Such a failure retains the exact automatic Codex session
recovery and emits only a prompt-free skip reason.

### Recovery Payload

```text
AutomaticRecovery::Codex {
  session_id: CodexSessionId,
  prompt_area: Option<CapturedCodexPromptArea>
}

derived argv = ["codex", "resume", <session-id>]
```

The executable name shown here is the v1 recovery command contract. Normal
restore preflight still resolves it from the invocation environment to one
absolute executable.
After launch, `RecoveredAutomatically` requires a pane-tied Codex session file
whose parsed `payload.id` equals the requested `CodexSessionId`. A foreground
Codex process without exact identity confirmation becomes `NeedsAttention`.

If the payload also contains a prompt area, the normal post-launch observation
must first recover that exact session. Prompt preparation then performs a second
fresh pane-tied classification and again requires the same exact
`CodexSessionId`; the earlier settle result is not reusable authorization. Only
that match permits one literal `set-buffer` plus bracketed
`paste-buffer -d -p -r` into the exact retained pane. It sends no Enter and does
not retry. A different session, missing pane, ownership loss, endpoint change,
or failed paste sends no prompt input where detectable and produces a
recovered-but-partial needs-attention result. The prompt text remains plaintext
inside the owner-only snapshot and is never included in inspection, plan,
warning, or result text.

## Claude Code

### Recognition

The foreground process must be a Claude Code interactive TUI tied to the pane.
Background agents and generic Claude Code service processes are not themselves
recoverable foreground sessions. v1 accepts only its understood interactive
global-option forms. Positional launches, unknown options, help/version,
print/background/worktree modes, subcommands, and `--fork-session` downgrade to
manual recovery. In particular, the UUID supplied with `--resume` identifies
the parent rather than the new fork and cannot authorize automatic recovery.

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
  transport: PtyIdentity
}
```

When using a worker record, every field above is mandatory. The working
directory must equal the pane evidence; PID and process start time must identify
the same live foreground worker; and the PTY identity must equal the pane's
terminal identity. A missing or mismatched field is insufficient or conflicting
evidence. Socket-bound worker records are not automatically recoverable in v1
because the snapshot boundary has no independent pane-to-socket proof. A daemon
roster or status record may corroborate these facts but cannot supply a missing
field, and daemon health alone does not identify the pane's session.

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
restore preflight still resolves it from the invocation environment to one
absolute executable.
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

Restore replaces captured `argv[0]` with the absolute target `mdbook` executable
proved by preflight and preserves every remaining argument exactly. Post-launch
success requires pane-tied foreground evidence that satisfies the same
recognition rules, the same `argv[0]` basename, and the exact captured arguments
after `argv[0]`.

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

Restore replaces captured `argv[0]` with the absolute target `book` executable
proved by preflight and preserves every remaining argument exactly. Post-launch
success requires pane-tied foreground evidence that satisfies the same
recognition rules, the same `argv[0]` basename, and the exact captured arguments
after `argv[0]`.
