# tmux-rescue

tmux-rescue snapshots one tmux server's session/window/pane topology and the
foreground command in each pane. It is designed to help reconstruct a workspace
after a tmux-server crash.

Inspection validates a stored snapshot and presents the workspace it contains
as a topology-first terminal tree. It is read-only and does not contact tmux or
inspect current processes.

Restore is plan-first: by default it validates the snapshot and prints the
planned topology and pane actions. `--run` performs that printed plan only when
the target tmux server is absent.

## Requirements

- Linux
- tmux 3.4

## Install

```bash
cargo install tmux-rescue
```

## Use

Capture the tmux server selected by the invoking tmux context:

```bash
tmux-rescue snapshot
```

Inspect the global latest snapshot:

```bash
tmux-rescue inspect
```

Or inspect an explicit immutable snapshot without using the global state root:

```bash
tmux-rescue inspect /path/to/immutable-snapshot.json --color never --icons nerd
```

The primary tree uses Nerd icons and requires a [Nerd Font Mono](https://www.nerdfonts.com/)
terminal font. The portable default is `--icons unicode`. The view keeps captured
values complete and calls out limitations inline while still showing the rest of
the workspace:

```text
Snapshot     latest
Captured     2026-07-24T05:31:32.581307924Z
Source       /tmp/tmux-1000/default
Consistency  ● stable topology
File         /home/user/.local/state/tmux-rescue/snapshots/80000000000000000000000000000000-550e8400-e29b-41d4-a716-446655440000.json

Contents     1 session · 1 window · 1 pane
Programs     1 shell

◆ work · 1 window · 1 pane
   /home/user/work
└─  0 editor ›  0 shell
       = ◆
```

An unstable topology warning is part of the same document; the complete
snapshot tree is still displayed successfully. Color defaults to automatic and
can be forced or disabled with `--color always` or `--color never`. A window
with one pane is compacted to one line; windows with multiple panes retain their
full branch structure.

Inspect a restore plan for an absent target socket:

```bash
tmux-rescue restore --target /tmp/tmux-rescue.sock
```

Execute the same plan only after reviewing it:

```bash
tmux-rescue restore --target /tmp/tmux-rescue.sock --run
```

The default snapshot is `latest` for both `inspect` and `restore`; an explicit
immutable snapshot path may be supplied as the first argument.

## Scope and safety

v1 captures manually. It recreates topology and working directories, but does
not restore exact pane layout, environment variables, process trees, scrollback,
or unsaved terminal input. It never restores into an existing tmux server.

Inspection performs no live tmux, process, restore-planning, or preflight work.

Only a closed automatic-recovery whitelist is executed automatically. Other
captured foreground commands are pasted as hints without pressing Enter.

The complete design and recovery contracts are in the published
[Design](https://huweiatgithub.github.io/tmux-rescue/DESIGN.html),
[Architecture](https://huweiatgithub.github.io/tmux-rescue/ARCHITECTURE.html),
and [Tool Recoveries](https://huweiatgithub.github.io/tmux-rescue/TOOL-RECOVERIES.html)
documentation.

## License

tmux-rescue is dedicated to the public domain under [CC0 1.0](LICENSE).
