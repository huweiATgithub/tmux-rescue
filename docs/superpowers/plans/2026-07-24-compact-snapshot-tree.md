# Compact Snapshot Tree Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make snapshot trees easier to scan by compacting exactly-one-pane
windows, distinguishing window and pane indexes, and offering the approved Nerd
Font labels through an explicit `--icons nerd` mode.

**Architecture:** Parse icon policy once into `IconMode`, carry it in
`InspectRequest`, and convert it at the renderer boundary into a private symbol
set. Tree shape comes only from the already-validated pane count: one pane is a
multiline window/pane breadcrumb; two or more panes retain their hierarchy.
Color remains an independent policy, and all renderer-owned glyphs remain
uncolored except the existing semantic session anchor.

**Tech Stack:** Rust 1.94, Clap 4.5 typed parsing, `termtree` 1.0, `anstyle`
1.0, binary integration tests, mdBook documentation.

## Constraints

- Work only in `/home/huwei/projects/tmux-rescue/.worktrees/snapshot-inspect`
  on `feat/snapshot-inspect`.
- Follow red-green-refactor for every behavior change. Record the focused test
  failure before editing production code.
- Default to `--icons unicode`; never attempt font detection.
- Unicode labels are `[N]` for windows, `(N)` for panes, and `cwd` for working
  directories. Nerd labels are ` N` (U+EB7F), ` N` (U+EBC8), and ``
  (U+EAF7).
- Use `window › pane` only for exactly one pane. Preserve the actual window and
  pane source indexes, including nonzero single-pane indexes.
- Keep multi-pane windows nested. Snapshot ordering, counts, program summaries,
  warnings, exit status, and trust boundaries do not change.
- Equal pane/session cwd bytes render as `cwd = ◆` or ` = ◆`; this is an
  equality reference, not inheritance. Differing paths remain complete.
- Use three renderer-owned spaces before pane detail lines. Let `termtree` add
  the structural continuation prefix.
- The Nerd glyphs use default foreground. Existing cyan `◆`, green `●`, yellow
  `▲`/`!`, red `error:`, and bold rules remain unchanged.

---

### Task 1: Parse And Carry A Typed Icon Mode

**Files:**
- Modify: `src/cli.rs`

**Interface:** Add `IconMode::{Unicode, Nerd}` and
`InspectRequest { selection, color, icons }`. The default is structurally
represented as `IconMode::Unicode`; downstream code never receives a raw icon
mode string.

- [ ] **Step 1: Add failing CLI parsing and dispatch assertions**

Extend `parses_the_exact_command_surface` to assert:

```rust
assert_eq!(
    inspect_command(parse(&["tmux-rescue", "inspect"])),
    (None, ColorMode::Auto, IconMode::Unicode),
);
assert_eq!(
    inspect_command(parse(&[
        "tmux-rescue", "inspect", "snapshot.json", "--icons", "nerd",
    ])),
    (
        Some(PathBuf::from("snapshot.json")),
        ColorMode::Auto,
        IconMode::Nerd,
    ),
);
assert!(Cli::try_parse_from([
    "tmux-rescue", "inspect", "--icons", "automatic",
]).is_err());
```

Extend the dispatch assertion so the recorded request contains
`icons: IconMode::Nerd`.

Run: `cargo test --locked --bin tmux-rescue cli::tests::parses_the_exact_command_surface`

Expected: compile failure because `IconMode` and the `icons` fields do not yet
exist.

- [ ] **Step 2: Implement the typed boundary**

Add a defaulted Clap `ValueEnum` and `--icons` field beside `--color`. Carry the
parsed value through `Command::Inspect`, `dispatch`, and `InspectRequest`.
Update all direct `InspectRequest` construction sites.

Run:

```bash
cargo test --locked --bin tmux-rescue cli::tests::parses_the_exact_command_surface
cargo test --locked --bin tmux-rescue cli::tests::dispatches_without_owning_orchestration
cargo test --locked --bin tmux-rescue cli::tests::inspect_output_failure_is_fatal_and_reports_on_stderr
```

Expected: all pass.

- [ ] **Step 3: Commit the parsed icon policy**

```bash
git add src/cli.rs
git commit -m "feat: parse snapshot icon mode"
```

### Task 2: Render Compact Trees In Both Icon Modes

**Files:**
- Modify: `src/inspect.rs`
- Modify: `src/cli.rs`

**Interface:** Change the renderer boundary to
`render(&LoadedSnapshot, &SnapshotSelection, Palette, IconMode) -> String`.
Immediately map `IconMode` to a private `TreeSymbols` value containing cwd text
and window/pane index delimiters or prefixes.

- [ ] **Step 1: Make the exact plain-tree test describe mixed topology**

Update `renders_complete_plain_snapshot_tree` first. Its existing fixture
already has:

- one two-pane window, which must remain nested with `(0)` and `(1)`;
- one one-pane window, which must become `[1] zsh › (0) ...`; and
- a second session whose one pane has source index `2`, which must become
  `[4] shell › (2) shell`.

All pane detail lines use three local spaces. Equal cwd output becomes
`cwd = ◆`.

Run: `cargo test --locked --bin tmux-rescue inspect::tests::renders_complete_plain_snapshot_tree`

Expected: assertion failure showing the old nested `[N]`/`cwd = session`
grammar.

- [ ] **Step 2: Add a failing exact Nerd Font rendering test**

Render the same fixture with `IconMode::Nerd` and assert the complete tree
portion, including these exact code points:

```text
◆ notes · 1 window · 1 pane
   /home/huwei/notes
└─  4 shell ›  2 shell
       = ◆
```

Also assert the expanded two-pane window uses ` 0`, then child nodes ` 0`
and ` 1`, rather than flattening.

Run: `cargo test --locked --bin tmux-rescue inspect::tests::renders_nerd_font_tree_with_selected_glyphs`

Expected: compile failure because the renderer does not accept `IconMode`.

- [ ] **Step 3: Implement symbols and conditional compaction**

Add private constants equivalent to:

```rust
const DETAIL_INDENT: &str = "   ";

struct IndexSymbols {
    prefix: &'static str,
    suffix: &'static str,
}

struct TreeSymbols {
    window: IndexSymbols,
    pane: IndexSymbols,
    cwd: &'static str,
}
```

Select `[`, `]`, `(`, `)`, and `cwd` for Unicode; select ` `, empty suffix,
` `, empty suffix, and `` for Nerd mode.

Refactor `PaneView` into small formatting operations for its first line and
details so `WindowView::tree` can reuse them. Match the validated pane slice:

- `[pane]`: build one multiline breadcrumb node and append that pane's details;
- two or more panes: build the window node and push normal multiline pane
  children; and
- `[]`: unreachable for `LoadedSnapshot`; make the impossible state explicit
  rather than silently producing a window without panes.

Use `palette.cyan("◆")` for both the session anchor and exact-cwd references so
the reference target keeps one semantic identity. Do not color Nerd glyphs.

Pass `request.icons` from `run_inspect` into `render` and update unit calls to
state their icon mode explicitly.

Run:

```bash
cargo test --locked --bin tmux-rescue inspect::tests::renders_complete_plain_snapshot_tree
cargo test --locked --bin tmux-rescue inspect::tests::renders_nerd_font_tree_with_selected_glyphs
cargo test --locked --bin tmux-rescue inspect::tests::forced_color_styles_only_approved_tokens
cargo test --locked --bin tmux-rescue inspect::tests::unstable_warning_keeps_the_complete_tree
```

Expected: all pass; forced-color output strips byte-for-byte to plain output.

- [ ] **Step 4: Run all renderer and CLI unit tests, then commit**

Run:

```bash
cargo test --locked --bin tmux-rescue inspect::tests
cargo test --locked --bin tmux-rescue cli::tests
```

Expected: all pass.

```bash
git add src/inspect.rs src/cli.rs
git commit -m "feat: render compact snapshot trees"
```

### Task 3: Verify The Compiled CLI And Update Its Documentation

**Files:**
- Modify: `tests/cli.rs`
- Modify: `README.md`
- Modify: `docs/src/DESIGN.md`
- Modify: `docs/src/ARCHITECTURE.md`

- [ ] **Step 1: Add a failing compiled-binary Nerd-mode test**

Add an integration test that runs an explicit stable fixture with
`--color never --icons nerd` and asserts exact output ending in:

```text
◆ work · 1 window · 1 pane
   /workspace
└─  0 editor ›  0 shell
       = ◆
```

Keep the existing default integration test in Unicode mode and update its end
assertion to the compact `[0] editor › (0) shell` form. Update the unstable
multi-pane assertions to `(0)` and `(1)` while preserving the full-warning and
continued-rendering proof.

Run: `cargo test --locked --test cli inspect`

Expected: failure until the compiled CLI surface and expected grammar agree.

- [ ] **Step 2: Make integration coverage pass**

Update only assertions affected by the approved grammar. Add an invalid
`--icons` binary assertion if invalid typed values are not already covered by
the parser test.

Run: `cargo test --locked --test cli inspect`

Expected: all inspection integration tests pass.

- [ ] **Step 3: Synchronize user-facing documentation**

Update the command surface in all three documents. Make README's primary tree
the polished `--icons nerd` example, state its Nerd Font Mono requirement, and
briefly name the portable default. Explain conditional one-pane compaction and
the cwd equality reference in `DESIGN.md`; record typed icon policy and private
symbol ownership in `ARCHITECTURE.md`. Do not change restore or snapshot
contracts.

Run:

```bash
rg -n "cwd = session|\[0\].*Codex|inspect \[SNAPSHOT\]" README.md docs/src/DESIGN.md docs/src/ARCHITECTURE.md
mdbook build docs
```

Expected: no stale inspect grammar; the book builds successfully.

- [ ] **Step 4: Commit integration coverage and docs**

```bash
git add tests/cli.rs README.md docs/src/DESIGN.md docs/src/ARCHITECTURE.md
git commit -m "docs: explain compact snapshot trees"
```

### Task 4: Run Release-Grade Verification And Update The PR

- [ ] **Step 1: Run formatting and static checks**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
git diff --check origin/main...HEAD
```

- [ ] **Step 2: Run the complete test and documentation matrix**

```bash
cargo test --all-targets --all-features --locked -- --test-threads=1
cargo build --release --locked
cargo doc --no-deps --locked
mdbook build docs
cargo package --list --locked
cargo package --locked
```

Expected: every command exits `0`.

- [ ] **Step 3: Request independent specification and code-quality reviews**

Give reviewers the design spec, this plan, and `origin/main...HEAD`. Address
only findings that trace to the approved feature, rerun affected focused tests,
then rerun the full matrix when production code changes.

- [ ] **Step 4: Push and refresh the existing draft PR**

Push `feat/snapshot-inspect`, confirm the draft PR still targets `main`, update
its summary/test evidence for the compact tree and `--icons nerd`, and inspect
hosted checks for the pushed head.
