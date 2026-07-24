# tmux-rescue

tmux-rescue snapshots one tmux server's session/window/pane topology and the
foreground command in each pane. It is designed to help reconstruct a workspace
after a tmux-server crash.

Inspection validates a stored snapshot and presents the workspace it contains
as a topology-first terminal tree. It is read-only and does not contact tmux or
inspect current processes.

Restore is plan-first: by default it validates the snapshot and prints the
planned topology and pane actions without contacting the destination or writing
application state. `--run` prints the same plan, then attempts to claim a fresh
destination without mutating an existing tmux server.

## Requirements

- Linux
- tmux 3.4

## Install

```bash
cargo install tmux-rescue
```

## Use

Capture the tmux server selected by the invoking tmux context, a named tmux
server, or an exact socket path:

```bash
tmux-rescue snapshot
tmux-rescue -L work snapshot
tmux-rescue -S ./work.sock snapshot
```

`-L` and `-S` are root options and must appear before `snapshot` or `restore`.
tmux-rescue passes the selected flag and value through to tmux without
resolving or normalizing them.

All captures share one archive: one `snapshots/` directory and one global
`latest` pointer. A capture from any selected server may advance that pointer;
there is no per-server snapshot stream.

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

Print restore plans for the global latest snapshot, using a named destination,
an exact destination socket path, or the selected snapshot's recorded source
path when no selector is given:

```bash
tmux-rescue -L rescue restore
tmux-rescue -S ./rescue.sock restore
tmux-rescue restore
```

Snapshot selection and destination selection are independent. To restore a
particular immutable capture to a selected destination, provide both:

```bash
tmux-rescue -L rescue restore /path/to/immutable-snapshot.json
```

Every plan begins by printing the exact selector it will use, for example
`target: -L rescue` or `target: -S ./rescue.sock`. With no explicit selector,
it prints the generated `-S` selector for the snapshot's recorded source path.
Display escaping is diagnostic; execution retains the original operating-
system string.

Plan-only restore does not contact the destination and performs no application
filesystem writes. After review, add `--run` to the same root-level form:

```bash
tmux-rescue -S ./rescue.sock restore /path/to/immutable-snapshot.json --run
```

Execution passes that exact selector to the one start-capable ownership claim.
Topology mutation begins only after the claim proves the attempt's token,
server PID, tmux server start time, operating-system process start time, and
zero sessions. A selector that reaches an existing server cannot establish the
new token, so tmux-rescue does not mutate or remove that server.

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
