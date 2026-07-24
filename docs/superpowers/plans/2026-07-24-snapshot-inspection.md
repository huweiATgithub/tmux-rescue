# Snapshot Inspection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only `tmux-rescue inspect [SNAPSHOT] [--color <auto|always|never>]` command that defaults to the validated global latest snapshot and prints the complete captured workspace as the approved polished Unicode tree.

**Architecture:** Keep snapshot selection and validation in `StateStore`, then pass only a refined `LoadedSnapshot`, a typed `SnapshotSelection`, and a resolved binary-local `Palette` into a private `inspect` renderer. The renderer first constructs private display facts, escapes all lossless operating-system values, computes ordered summaries, and finally gives only owned display strings to `termtree`; the reusable library remains unaware of terminal presentation. `anstream` resolves automatic color support at the real stdout and stderr boundaries, while `anstyle` emits the small fixed semantic palette.

**Tech Stack:** Rust 1.94, edition 2024, Clap 4.5 typed parsing, existing `StateStore` and opaque snapshot model, `termtree` 1.0, `anstyle` 1.0, `anstream` 1.0, unit and binary integration tests, mdBook documentation.

## Global Constraints

- Work only in the user-approved worktree `/home/huwei/projects/tmux-rescue/.worktrees/snapshot-inspect` on branch `feat/snapshot-inspect`.
- Follow red-green-refactor for every production behavior: add a focused failing test, run it and observe the expected failure, implement the minimum behavior, then rerun the focused test.
- Inspection is read-only. It may call only `StateStore::from_environment`, `StateStore::load_latest`, and `StateStore::load_explicit_path`; it must not construct `TmuxAdapter`, `LinuxProcessInspector`, a restore plan, an executor, or a preflight environment.
- `SnapshotSelection` and `ColorMode` are parsed/refined boundary types. The renderer accepts `LoadedSnapshot`, never JSON, `RawSnapshot`, or an unvalidated deserialized value.
- A valid unstable snapshot is successful output: print `▲ unstable topology after N attempts`, render the entire tree, write nothing to stderr, and return exit code `0`.
- A pane whose program was unavailable is also successful output: print `! program not captured` plus its stored reason and continue with every later pane and session.
- Do not generate the recovery-planner labels `Automatic`, `Manual`, or synonyms. Matching words originating in user-controlled snapshot content remain visible and complete.
- Color is redundant and token-local: cyan `◆`, green `●`, yellow `▲` and `!`, red `error:`, and bold only for the selected snapshot, session names, window names, pane facts, and the unstable warning phrase. Use no other effects or colors and never style connectors.
- Every enabled style ends immediately with an ANSI reset. Stripping ANSI from forced-color output must be byte-identical to plain output.
- Escape snapshot bytes before adding renderer-owned ANSI. Preserve printable Unicode; double literal backslashes, escape quotation marks and controls, and render invalid UTF-8 bytes as lowercase `\xNN`.
- Preserve argv boundaries. Quote empty arguments and arguments containing whitespace, quotes, backslashes, controls, or invalid UTF-8; this is a diagnostic representation, not a shell command.
- Show full timestamps, paths, IDs, commands, names, and reasons. Do not canonicalize explicit display paths, abbreviate home directories, truncate, wrap deliberately, page, inspect terminal width, or add ASCII/JSON modes.
- Use singular count nouns only when the count is exactly one; otherwise use the approved plural forms. Program entries remain in first-seen tree order and coalesce by identical visible identity.
- Existing snapshot, restore, and exit-code behavior is out of scope and must remain byte-compatible except for the new command appearing in Clap help.

## File Map

- `Cargo.toml`: add direct presentation dependencies `anstream`, `anstyle`, and `termtree`.
- `Cargo.lock`: lock `termtree` 1.0 and record the presentation crates as direct package dependencies.
- `src/main.rs`: register the binary-private renderer and measure stdout/stderr automatic color capability at the real locked streams.
- `src/cli.rs`: define typed inspect arguments, selection and color requests, dispatch inspection, load through `StateStore`, enforce stream/exit contracts, and render the fatal prefix.
- `src/inspect.rs`: own private display-view types, lossless value/argv encoding, counts, ordered program aggregation, `termtree` construction, and semantic styles.
- `tests/cli.rs`: exercise the compiled binary for explicit/latest selection, color policy, unstable success, invalid input, stream separation, and live-system independence.
- `README.md`: introduce the default and explicit inspection forms with the tree’s role.
- `docs/src/DESIGN.md`: add inspection to the v1 user experience and non-goals.
- `docs/src/ARCHITECTURE.md`: document selection/trust, renderer ownership, color/stream behavior, and exit status.

---

### Task 1: Parse A Refined Inspect Request

**Files:**
- Modify: `src/cli.rs`

**Interfaces:**
- Produces: `ColorMode::{Auto, Always, Never}`, `SnapshotSelection::{Latest, Explicit(PathBuf)}`, and `InspectRequest { selection, color }`.
- Extends: `CliRunner::inspect(&mut self, InspectRequest)` and `dispatch`.
- Invariant: callers cannot represent an inspect request with an ambiguous `Option<PathBuf>` after dispatch.

- [ ] **Step 1: Add failing parsing tests for the exact command surface**

Add a test helper that matches `Command::Inspect`, converts it through the same request constructor used by `dispatch`, and asserts these exact cases:

```rust
assert_eq!(
    inspect_request(parse(&["tmux-rescue", "inspect"])),
    InspectRequest {
        selection: SnapshotSelection::Latest,
        color: ColorMode::Auto,
    }
);
assert_eq!(
    inspect_request(parse(&[
        "tmux-rescue",
        "inspect",
        "relative/snapshot.json",
        "--color",
        "always",
    ])),
    InspectRequest {
        selection: SnapshotSelection::Explicit(PathBuf::from(
            "relative/snapshot.json"
        )),
        color: ColorMode::Always,
    }
);
assert!(Cli::try_parse_from([
    "tmux-rescue",
    "inspect",
    "--color",
    "sometimes",
])
.is_err());
```

Also parse `never`, accept `--color` before or after `SNAPSHOT`, reject a second positional path, and keep the existing snapshot/restore rejections.

Run: `cargo test --bin tmux-rescue cli::tests::parses_the_exact_command_surface --locked`  
Expected: failure because `Command::Inspect`, `InspectRequest`, `SnapshotSelection`, and `ColorMode` do not exist.

- [ ] **Step 2: Implement typed parsing and request refinement**

Use the following boundary types and do not pass the original optional path downstream:

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotSelection {
    Latest,
    Explicit(PathBuf),
}

impl From<Option<PathBuf>> for SnapshotSelection {
    fn from(path: Option<PathBuf>) -> Self {
        match path {
            Some(path) => Self::Explicit(path),
            None => Self::Latest,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectRequest {
    pub selection: SnapshotSelection,
    pub color: ColorMode,
}
```

Add this Clap surface:

```rust
/// Validate and display a captured tmux workspace.
Inspect {
    /// Immutable snapshot path. The global latest snapshot is used when omitted.
    snapshot: Option<PathBuf>,
    /// When to use terminal color.
    #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,
},
```

`dispatch` must immediately refine `snapshot.into()` and call `runner.inspect(request)`.

Run: `cargo test --bin tmux-rescue cli::tests::parses_the_exact_command_surface --locked`  
Expected: pass.

- [ ] **Step 3: Add and satisfy a focused dispatch test**

Extend `RecordingRunner` with `inspect_requests: Vec<InspectRequest>` and `inspect_code`. Assert that dispatching `inspect /state/one.json --color never` records exactly one explicit request and returns `inspect_code` without loading a file.

Run: `cargo test --bin tmux-rescue cli::tests::dispatches_without_owning_orchestration --locked`  
Expected: pass after extending the trait and all match arms exhaustively.

- [ ] **Step 4: Commit the parsed command boundary**

```bash
git add src/cli.rs
git commit -m "feat: parse snapshot inspection requests"
```

### Task 2: Encode Snapshot Values And Build Display Facts

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `src/inspect.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `Palette::{plain, colored}`, `render(&LoadedSnapshot, &SnapshotSelection, Palette) -> String`, and private `InspectView`, `SessionView`, `WindowView`, `PaneView`, and `ProgramEntry` types.
- Consumes: only read-only accessors on `LoadedSnapshot` and `ValidatedSnapshot`.
- Invariant: all `String` fields in the private view are terminal-safe display encodings; raw path/argument bytes never reach `termtree` or `anstyle`.

- [ ] **Step 1: Add the presentation dependencies and renderer module**

Add exact direct dependency requirements:

```toml
anstream = "1.0"
anstyle = "1.0"
termtree = "1.0"
```

Register `mod inspect;` only in `src/main.rs`; do not export it from `src/lib.rs`. Resolve the lockfile with:

Run: `cargo check --locked`  
Expected: lockfile failure because `termtree` is not yet locked.

Run: `cargo check`  
Expected: dependency resolution succeeds and records `termtree` 1.0.x.

- [ ] **Step 2: Write failing lossless display-encoding tests**

In `src/inspect.rs`, load fixtures through `StateStore::load_explicit_path` so tests cannot construct an unvalidated view. Test the byte encoder and argv renderer with these exact inputs and outputs:

```rust
assert_eq!(display_bytes(b"/tmp/plain"), "/tmp/plain");
assert_eq!(display_bytes("/tmp/数据".as_bytes()), "/tmp/数据");
assert_eq!(display_bytes(b"quote\"slash\\tab\t"), "quote\\\"slash\\\\tab\\t");
assert_eq!(display_bytes(b"escape\x1b"), "escape\\x1b");
assert_eq!(display_bytes(&[b'f', 0x80, b'o']), "f\\x80o");

assert_eq!(display_argv(&[]), "");
assert_eq!(display_argv(&[os(b"book"), os(b"serve")]), "book serve");
assert_eq!(
    display_argv(&[
        os(b"cmd"),
        os(b""),
        os(b"two words"),
        os(b"quote\""),
        os(&[0x80]),
    ]),
    "cmd \"\" \"two words\" \"quote\\\"\" \"\\x80\""
);
```

The empty-slice case is a helper boundary test; validated `CapturedCommand::argv()` itself is never empty.

Run: `cargo test --bin tmux-rescue inspect::tests::encodes_lossless_values_without_terminal_controls --locked`  
Expected: compile failure because `src/inspect.rs` has no implementation.

- [ ] **Step 3: Implement the terminal-safe encoded value type**

Use a private type rather than validating and then retaining raw bytes:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
struct DisplayValue(String);

impl DisplayValue {
    fn from_bytes(bytes: &[u8]) -> Self;
    fn from_os(value: &OsStr) -> Self;
    fn as_str(&self) -> &str;
}
```

Walk maximal valid UTF-8 prefixes with `std::str::from_utf8`. For valid characters, preserve printable Unicode, emit `\\`, `\"`, `\n`, `\r`, and `\t`, and render every other control as `\xNN` when it is one byte or `\u{hex}` otherwise. For each invalid byte in `Utf8Error::error_len()` (or the remaining suffix when it is `None`), emit lowercase `\xNN`. Determine argument quoting from the original bytes so invalid UTF-8 and Unicode whitespace cannot be mistaken for safe bare arguments.

Run: `cargo test --bin tmux-rescue inspect::tests::encodes_lossless_values_without_terminal_controls --locked`  
Expected: pass.

- [ ] **Step 4: Write failing typed-view mapping tests**

Load one validated fixture containing, in order, idle, Codex, Claude Code, mdBook serve, Bookshelf serve, ordinary captured command, and unavailable panes. Assert:

- pane facts are exactly `shell`, `Codex`, `Claude Code`, captured argv text, and `program not captured`;
- tool sessions retain their UUIDs;
- captured commands retain their executable display values;
- `cwd = session` appears only for byte-equal paths;
- a different and a non-UTF-8 pane path remains complete;
- the generated labels contain neither `Automatic` nor `Manual`, while user data named `automatic` and arguments containing `manual` remain unchanged.

Run: `cargo test --bin tmux-rescue inspect::tests::maps_recovery_variants_to_user_facts --locked`  
Expected: failure because `InspectView::from_loaded` is absent.

- [ ] **Step 5: Implement private fact mapping and ordered aggregation**

Map the four model cases exhaustively:

```rust
match pane.recovery() {
    PaneRecovery::Idle => PaneFact::Shell,
    PaneRecovery::Automatic(AutomaticRecovery::Codex { session_id }) => { /* Codex */ }
    PaneRecovery::Automatic(AutomaticRecovery::ClaudeCode { session_id }) => { /* Claude Code */ }
    PaneRecovery::Automatic(AutomaticRecovery::MdBookServe { command }) => { /* command.command() */ }
    PaneRecovery::Automatic(AutomaticRecovery::BookshelfServe { command }) => { /* command.command() */ }
    PaneRecovery::Manual(command) => { /* captured command */ }
    PaneRecovery::Unavailable(failure) => { /* stored reason */ }
}
```

Use a `Vec<ProgramEntry>` and update the first entry whose encoded visible identity matches; do not sort. Command identities come from `Path::file_name()` on the captured executable, falling back to the complete encoded executable. Coalesce identical rendered identities, including an idle `shell` and a command executable whose basename is literally `shell`. Inflect only the visible `shell` identity when its total is not one.

Run: `cargo test --bin tmux-rescue inspect::tests::maps_recovery_variants_to_user_facts --locked`  
Expected: pass.

- [ ] **Step 6: Commit encoded display facts**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/inspect.rs
git commit -m "feat: map snapshots to inspect display facts"
```

### Task 3: Render The Complete Plain Unicode Tree

**Files:**
- Modify: `src/inspect.rs`

**Interfaces:**
- Completes: `render(&LoadedSnapshot, &SnapshotSelection, Palette::plain()) -> String`.
- Uses: `termtree::Tree<String>` and a fixed `GlyphPalette` with `├─`, `└─`, and `│` geometry.
- Invariant: topology order is identical to validated snapshot order and no width-dependent operation exists.

- [ ] **Step 1: Add a failing exact stable-output test**

Use an explicit relative fixture path and at least two sessions, two windows under the first session, and multiple panes. Assert the whole string, including blank lines, indentation, and final newline, begins and continues exactly as follows for the fixture’s values:

```text
Snapshot     explicit
Captured     2026-07-24T05:31:32.581307924+08:00
Source       /tmp/tmux-1000/default
Consistency  ● stable topology
File         relative/snapshot.json

Contents     2 sessions · 3 windows · 4 panes
Programs     1 Codex · 2 shells · 1 not captured

◆ MetaNC · 2 windows · 3 panes
  cwd /home/huwei/projects/MetaNC
├─ [0] node
│  ├─ [0] Codex
│  │       session 019f7ac5-a55c-7e70-8b31-872ae70c9a94
│  │       cwd = session
│  └─ [1] shell
│          cwd = session
└─ [1] zsh
   └─ [0] ! program not captured
           reason foreground process disappeared
           cwd /home/huwei/projects/MetaNC/.worktrees/inspect

◆ notes · 1 window · 1 pane
  cwd /home/huwei/notes
└─ [4] shell
   └─ [2] shell
           cwd = session
```

Adjust the fixture itself, not the asserted grammar, so the program and topology counts agree exactly. The test must also assert that the explicit relative `File` value was not canonicalized.

Run: `cargo test --bin tmux-rescue inspect::tests::renders_complete_plain_snapshot_tree --locked`  
Expected: failure because the renderer has no header/tree formatting.

- [ ] **Step 2: Implement header, summaries, and termtree geometry**

Use one independently rendered `Tree<String>` per session, separated by one blank line. Configure this exact palette on each session tree:

```rust
const TREE_GLYPHS: termtree::GlyphPalette = termtree::GlyphPalette {
    middle_item: "├",
    last_item: "└",
    item_indent: "─ ",
    middle_skip: "│",
    last_skip: " ",
    skip_indent: "  ",
};
```

Build each window as a child, each pane as its child, and set multiline mode on pane nodes so `termtree` supplies every continuation connector. Keep the extra four-space detail indentation inside each pane’s display string. Render session cwd as the second line of the root string. Use a single helper for `session(s)`, `window(s)`, and `pane(s)` so count grammar cannot diverge.

Run: `cargo test --bin tmux-rescue inspect::tests::renders_complete_plain_snapshot_tree --locked`  
Expected: pass with the exact approved connectors and whitespace.

- [ ] **Step 3: Add and satisfy an exact unstable-output regression**

Load a valid snapshot with `{"kind":"unstable","attempts":3}`. Assert the exact header contains:

```text
Consistency  ▲ unstable topology after 3 attempts
```

and that the final pane and final session are both still present. The output remains a complete document; do not return or branch away after formatting the warning.

Run: `cargo test --bin tmux-rescue inspect::tests::unstable_warning_keeps_the_complete_tree --locked`  
Expected: pass after representing consistency as a display fact rather than an error.

- [ ] **Step 4: Cover timestamp, path, aggregation, and hostile-value edges**

Add exact-output tests for:

- preserved RFC 3339 fractional precision and non-UTC offset;
- latest selection showing `Snapshot     latest` and the complete absolute selected file;
- first-seen program order where a later repeated identity increments the earlier entry;
- executable `/` falling back to its full display value;
- empty and quoted arguments, Unicode, literal backslashes, controls, and invalid UTF-8;
- user names/arguments containing `automatic`, `manual`, and the bytes `ESC [ 31 m` without producing an actual escape byte;
- byte-equal and byte-different cwd values whose human-readable prefixes are similar.

Run: `cargo test --bin tmux-rescue inspect::tests --locked`  
Expected: all plain renderer tests pass.

- [ ] **Step 5: Commit the plain tree**

```bash
git add src/inspect.rs
git commit -m "feat: render snapshot inspection tree"
```

### Task 4: Apply The Fixed Palette And Stream Policy

**Files:**
- Modify: `src/inspect.rs`
- Modify: `src/cli.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `TerminalColorSupport { stdout_auto, stderr_auto }` and `ColorMode::enabled(auto_support) -> bool`.
- Extends: `SystemCliRunner::with_color_support(stdout, stderr, support)` while retaining `new` for buffer-based tests with automatic color disabled.
- Invariant: explicit `always` and `never` override `TerminalColorSupport`; automatic stdout and stderr capability are resolved independently.

- [ ] **Step 1: Add failing exact styled-output tests**

Render the stable and unstable fixtures with `Palette::colored()` and assert hard-coded ANSI sequences around only these tokens:

```text
\x1b[1mlatest\x1b[0m
\x1b[32m●\x1b[0m stable topology
\x1b[33m▲\x1b[0m \x1b[1munstable topology after 3 attempts\x1b[0m
\x1b[36m◆\x1b[0m \x1b[1mMetaNC\x1b[0m
\x1b[1mnode\x1b[0m
\x1b[1mCodex\x1b[0m
\x1b[33m!\x1b[0m \x1b[1mprogram not captured\x1b[0m
```

Assert no ANSI sequence touches a connector, count, index, path, UUID, executable, reason, `stable topology` phrase, or summary entry. Pass the colored bytes through `anstream::StripStream<Vec<u8>>` and assert exact equality with `Palette::plain()` output.

Run: `cargo test --bin tmux-rescue inspect::tests::forced_color_styles_only_approved_tokens --locked`  
Expected: failure because `Palette::colored()` is not yet applied.

- [ ] **Step 2: Implement token-local styling with `anstyle`**

Define only these styles:

```rust
const BOLD: anstyle::Style = anstyle::Style::new().bold();
const CYAN: anstyle::Style = anstyle::AnsiColor::Cyan.on_default();
const GREEN: anstyle::Style = anstyle::AnsiColor::Green.on_default();
const YELLOW: anstyle::Style = anstyle::AnsiColor::Yellow.on_default();
const RED: anstyle::Style = anstyle::AnsiColor::Red.on_default();
```

`Palette::paint(style, text)` must return the original text for plain output and `style + text + style.render_reset()` for colored output. Apply styles only after every untrusted value has become a `DisplayValue`. Expose a small `Palette::fatal_prefix()` for `src/cli.rs`; do not duplicate red ANSI literals there.

Run: `cargo test --bin tmux-rescue inspect::tests::forced_color_styles_only_approved_tokens --locked`  
Expected: pass, including stripped equality.

- [ ] **Step 3: Add failing color-resolution and fatal-prefix tests**

In `src/cli.rs`, assert this complete truth table:

```rust
assert!(!ColorMode::Auto.enabled(false));
assert!(ColorMode::Auto.enabled(true));
assert!(ColorMode::Always.enabled(false));
assert!(ColorMode::Always.enabled(true));
assert!(!ColorMode::Never.enabled(false));
assert!(!ColorMode::Never.enabled(true));
```

Use buffer sinks and forced support values to assert stdout and stderr are independent. A failed inspect with colored stderr must begin `\x1b[31merror:\x1b[0m `; plain stderr begins `error: `. Existing snapshot and restore failures must remain plain.

Run: `cargo test --bin tmux-rescue cli::tests::inspect_color_policy_resolves_per_stream --locked`  
Expected: failure because the runner has no color support.

- [ ] **Step 4: Measure real stream support with `anstream`**

After locking stdout and stderr in `main`, call `anstream::AutoStream::choice(&stdout)` and `choice(&stderr)`. Convert each effective result to a boolean with `choice != anstream::ColorChoice::Never`, then construct:

```rust
let support = TerminalColorSupport::new(stdout_auto, stderr_auto);
let mut runner = SystemCliRunner::with_color_support(
    &mut stdout,
    &mut stderr,
    support,
);
```

Do not evaluate environment variables directly or cache a process-global color decision in the renderer; `anstream` owns `NO_COLOR`, `CLICOLOR`, terminal, CI, and platform detection. `ColorMode::Always` still emits styles when the measured automatic choice is `Never`, while `Never` emits none when it is `Always`.

Run: `cargo test --bin tmux-rescue cli::tests::inspect_color_policy_resolves_per_stream --locked`  
Expected: pass.

- [ ] **Step 5: Commit palette and color policy**

```bash
git add src/inspect.rs src/cli.rs src/main.rs
git commit -m "feat: color snapshot inspection output"
```

### Task 5: Load And Print Inspection Documents

**Files:**
- Modify: `src/cli.rs`
- Modify: `tests/cli.rs`

**Interfaces:**
- Produces: `run_inspect(&InspectRequest, &mut impl Write, Palette) -> Result<u8, CliError>`.
- Extends: `SystemCliRunner::inspect` to select stdout and stderr palettes independently.
- Stream contract: successful stable or unstable inspection writes one complete document to stdout and nothing to stderr; a load/validation/write failure returns `1`, writes no successful document, and reports one terminal-safe error on stderr.

- [ ] **Step 1: Add failing explicit-inspection integration test**

Write a valid explicit fixture and execute:

```rust
let output = binary()
    .arg("inspect")
    .arg(&snapshot)
    .args(["--color", "never"])
    .env_remove("XDG_STATE_HOME")
    .env_remove("HOME")
    .env("TMUX", "/definitely/not/a/tmux/socket")
    .env("PATH", "/definitely/no/programs")
    .output()
    .unwrap();
```

Assert exit `0`, empty stderr, exact header/tree facts on stdout, and no complaint about HOME, tmux, processes, a target, or preflight.

Run: `cargo test --test cli explicit_inspect_bypasses_state_root_and_live_systems --locked`  
Expected: failure because `SystemCliRunner::inspect` does not load or render.

- [ ] **Step 2: Implement selection, rendering, and one-document output**

Select exactly once:

```rust
let loaded = match &request.selection {
    SnapshotSelection::Latest => StateStore::from_environment()
        .map_err(|error| CliError::new(format!("open state store: {error}")))?
        .load_latest(),
    SnapshotSelection::Explicit(path) => StateStore::load_explicit_path(path),
}
.map_err(|error| CliError::new(format!("load snapshot: {error}")))?;
```

Build the complete `String` before the first stdout write, then `write_all` and `flush`. Return `EXIT_SUCCESS` for either consistency variant. In `SystemCliRunner::inspect`, resolve the stdout palette before calling `run_inspect`; on error, resolve the stderr palette and write only its `fatal_prefix()` plus the existing `safe_text` message. Keep `report_failure` unchanged for snapshot/restore.

Run: `cargo test --test cli explicit_inspect_bypasses_state_root_and_live_systems --locked`  
Expected: pass.

- [ ] **Step 3: Add latest, unstable, and unavailable integration tests**

Create `$XDG_STATE_HOME/tmux-rescue/snapshots/<name>.json` and a relative `latest` symlink using the existing storage layout. Assert:

- no positional path selects latest and displays the complete absolute snapshot file;
- a stable latest snapshot exits `0`, stdout contains its final pane, and stderr is empty;
- an unstable latest snapshot exits `0`, stdout contains the yellow-free warning in `--color never` and its final pane/session, and stderr is empty;
- an unavailable pane is followed by the next pane in stdout and does not alter exit status.

Run: `cargo test --test cli inspect --locked`  
Expected: pass.

- [ ] **Step 4: Add fatal load/validation and output-failure tests**

At binary level, cover a missing explicit file, missing latest pointer, escaping/dangling latest pointer, and malformed or invalid snapshot. For each, assert exit `1`, stdout is empty, stderr starts `error: load snapshot:` (or `error: open state store:` where applicable), and no tree header appears.

At unit level, pass a `Write` implementation that always returns `BrokenPipe` into `run_inspect`; assert it returns `CliError("write CLI output: ...")`. Then exercise `SystemCliRunner::inspect` with a writable stderr to assert exit `1` and best-effort error reporting.

Run: `cargo test --test cli --locked`  
Expected: all inspection integration cases pass.

- [ ] **Step 5: Add compiled-binary color-policy tests**

Because `Command::output` redirects streams, assert:

- omitted `--color` produces no escape bytes;
- `--color always` produces the approved escapes through redirection;
- `--color never` produces no escapes even with `CLICOLOR_FORCE=1`;
- auto with `CLICOLOR_FORCE=1` produces escapes;
- auto with `NO_COLOR=1` produces no escapes;
- stripping the `always` stdout equals the `never` stdout exactly;
- an invalid explicit snapshot with `--color always` colors only the fatal `error:` prefix on stderr and leaves stdout empty.

Run: `cargo test --test cli inspect_color --locked`  
Expected: pass.

- [ ] **Step 6: Commit CLI orchestration and integration coverage**

```bash
git add src/cli.rs tests/cli.rs
git commit -m "feat: inspect latest and explicit snapshots"
```

### Task 6: Document Snapshot Inspection At The Right Levels

**Files:**
- Modify: `README.md`
- Modify: `docs/src/DESIGN.md`
- Modify: `docs/src/ARCHITECTURE.md`

**Interfaces:**
- README role: let a new user discover, invoke, and interpret the command.
- Design role: state the v1 human experience and its deliberate boundaries.
- Architecture role: make selection/trust, presentation ownership, streams, and exit behavior normative for maintainers.

- [ ] **Step 1: Add README usage and a representative plain tree**

Document both commands exactly:

```bash
tmux-rescue inspect
tmux-rescue inspect /path/to/immutable-snapshot.json --color never
```

Explain that the default is the global latest snapshot, inspection validates but never contacts tmux or changes state, and an unstable warning does not suppress the captured workspace. Include one short plain-text tree containing a Codex pane, a shell pane, and `! program not captured`; do not introduce `Automatic` or `Manual` presentation categories.

- [ ] **Step 2: Update the v1 design authority**

In `docs/src/DESIGN.md`, add inspection beside snapshot and restore. State topology-first output, full values, stable/unstable facts, unavailable-pane continuation, fixed redundant color, latest-by-default selection, and the non-goals of JSON, filtering, TUI, width-aware truncation, and current-machine preflight.

- [ ] **Step 3: Update the architecture authority**

In `docs/src/ARCHITECTURE.md`, document this flow:

```text
latest or explicit path
    -> StateStore validated load
    -> LoadedSnapshot
    -> binary-private InspectView
    -> termtree + anstyle
    -> one stdout document
```

State exit `0` for stable, unstable, and unavailable-pane snapshots; exit `1` with empty successful stdout for selection/load/validation/render/write failures; and no stderr on valid inspection. Explain that `anstream` resolves auto support per stream and explicit color policy wins. Leave `docs/src/TOOL-RECOVERIES.md` untouched.

- [ ] **Step 4: Verify documentation and commit**

Run: `mdbook build docs`  
Expected: successful book build with no broken local references.

```bash
git add README.md docs/src/DESIGN.md docs/src/ARCHITECTURE.md
git commit -m "docs: explain snapshot inspection"
```

### Task 7: Verify, Review, And Prepare The Pull Request

**Files:**
- Inspect only: all changed files and generated package contents.
- Modify only if a verification failure traces directly to this feature.

**Interfaces:**
- Produces: a clean, reviewable `feat/snapshot-inspect` branch and a draft pull request against `main`.
- Invariant: no claim of success precedes fresh command output; no unrelated user changes enter a commit.

- [ ] **Step 1: Run formatting and focused static checks**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
```

Expected: both exit `0` with no diagnostics.

- [ ] **Step 2: Run the full locked test suite**

```bash
cargo test --all-targets --all-features --locked
```

Expected: all existing 138 baseline tests plus every new inspect test pass; no ignored failure is treated as proof.

- [ ] **Step 3: Verify docs, build, and package contents**

```bash
cargo build --locked
mdbook build docs
cargo package --locked
```

Expected: every command exits `0`; the package includes `src/inspect.rs` through the existing `src/**` rule and excludes worktree metadata and build output.

- [ ] **Step 4: Review the implementation against the approved design**

Inspect `git diff main...HEAD` and mechanically check:

- no renderer path accepts raw JSON or a raw snapshot type;
- no inspect path references tmux/process/restore-planning capabilities;
- no renderer-owned `Automatic` or `Manual` label exists;
- forced-color stripping equals plain output;
- unstable and unavailable fixtures reach the final tree node with exit `0`;
- every changed line traces to inspection, documentation, or its direct dependency/test support.

Run: `rg -n "TODO|TBD|FIXME|placeholder" src/inspect.rs src/cli.rs tests/cli.rs README.md docs/src/DESIGN.md docs/src/ARCHITECTURE.md`  
Expected: no newly introduced unresolved marker.

- [ ] **Step 5: Request a code review and address only evidence-backed findings**

Invoke `superpowers:requesting-code-review` with the approved design, this plan, the branch diff, and fresh verification output. Reproduce any reported defect before changing code; rerun the focused test and the full verification gate after a fix.

- [ ] **Step 6: Confirm branch scope and publish the draft PR**

```bash
git status --short --branch
git log --oneline --decorate origin/main..HEAD
git diff --stat origin/main...HEAD
git push -u origin feat/snapshot-inspect
```

Use `github:yeet` to open a draft PR against `main`. The PR body must summarize the topology-first view and palette, call out that unstable snapshots still render successfully, list the exact verification commands, and state that inspection performs no live tmux/process/preflight access. Do not merge the PR.
