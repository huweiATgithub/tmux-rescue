# tmux-rescue

tmux-rescue snapshots one tmux server's session/window/pane topology and the
foreground command in each pane. It is designed to help reconstruct a workspace
after a tmux-server crash.

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

Inspect a restore plan for an absent target socket:

```bash
tmux-rescue restore --target /tmp/tmux-rescue.sock
```

Execute the same plan only after reviewing it:

```bash
tmux-rescue restore --target /tmp/tmux-rescue.sock --run
```

The default snapshot is `latest`; an explicit immutable snapshot path may be
supplied as the first argument to `restore`.

## Scope and safety

v1 captures manually. It recreates topology and working directories, but does
not restore exact pane layout, environment variables, process trees, scrollback,
or unsaved terminal input. It never restores into an existing tmux server.

Only a closed automatic-recovery whitelist is executed automatically. Other
captured foreground commands are pasted as hints without pressing Enter.

The complete design and recovery contracts are in the published
[Design](https://huweiatgithub.github.io/tmux-rescue/DESIGN.html),
[Architecture](https://huweiatgithub.github.io/tmux-rescue/ARCHITECTURE.html),
and [Tool Recoveries](https://huweiatgithub.github.io/tmux-rescue/TOOL-RECOVERIES.html)
documentation.

## License

tmux-rescue is dedicated to the public domain under [CC0 1.0](LICENSE).
