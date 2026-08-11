# dockyard

A terminal UI for managing Docker — containers, images, volumes and networks — built with
[ratatui](https://ratatui.rs) and [bollard](https://docs.rs/bollard).

```
╭ dockyard ──────────────────────────────────── docker 29.7.2  ·  20/21 running ╮
│ ▸ Containers 21     Images 41     Volumes 27     Networks 8                   │
╰───────────────────────────────────────────────────────────────────────────────╯
╭ containers  21  ·  name ▾ ────────────────────────────────────────────────────╮
│   NAME                   IMAGE                    STATE          CPU      MEM  │
│ ● argilla-postgres-1     postgres:14              Up 2 days     0.0%   15.6MB  │
│ ● evalm8-server          …divyam-evalm8:dev-late… Up 2 days     2.0%    158MB  │
│ ○ lakefs-setup           curlimages/curl:latest   Exited (0)       -        -  │
╰───────────────────────────────────────────────────────────────────────────────╯
╭ logs · evalm8-server  ·  following ───────────────────────────────────────────╮
│ 11:09:43 INFO  server listening on :9000                                       │
╰───────────────────────────────────────────────────────────────────────────────╯
 ↑↓ move   tab switch   ⏎ details   l hide logs   S stop   s shell   / filter   ?
```

## What it does

- **Live everything.** Container state updates the moment it changes — dockyard subscribes to
  the daemon's own event stream rather than only polling. CPU, memory, network and disk figures
  stream continuously, computed the same way `docker stats` computes them.
- **Logs inline.** The selected container's logs follow in the bottom pane, with ANSI colours
  preserved, timestamps converted to your local timezone, and scroll/search/wrap.
- **A real shell.** Press `s` and dockyard hands the terminal to `bash` (or `ash`, or `sh` —
  whatever the image has) running inside the container, forwards window resizes, and takes the
  terminal back cleanly when you exit.
- **Lifecycle actions** with a confirmation step on anything destructive, an in-row spinner
  while a job runs, and the daemon's own error text if it refuses.
- **Stacks, folded.** `z` groups containers by their compose project (or swarm stack), read from
  the labels rather than guessed from the name. Each header says how much of the stack is up,
  rows shorten to their service name, `space` folds one and `Z` folds the lot. Containers nobody
  deployed collect in a `standalone` bucket at the bottom.
- **Fuzzy filter** (`/`) and a **command palette** (`:`) over every command dockyard has.
- **Mouse, if you want it.** Click tabs and rows, double-click for details, click a column
  header to sort by it, wheel over whichever pane you're pointing at. Off with `--no-mouse`.

## Install

```sh
cargo install --path .
```

Requires Rust 1.86+ and access to a Docker daemon.

## Usage

```sh
dockyard                              # auto-detect: $DOCKER_HOST, else the local socket
dockyard --host tcp://10.0.0.5:2375   # a remote daemon
dockyard --interval 1000              # poll every second instead of every two
```

### Appearance

| Flag | Effect |
|---|---|
| `--ascii` | Pure 7-bit ASCII — borders, status marks, key names. Also `DOCKYARD_ASCII=1`. |
| `--icons` | Nerd Font icons in the tab bar. Also `DOCKYARD_ICONS=1`. |
| `--ansi` | Use the terminal's own 16 colours instead of dockyard's palette. |
| `--no-color` | No colour at all. `NO_COLOR` is honoured automatically. |
| `--no-mouse` | Don't capture the mouse. |

The default is Unicode geometric shapes (`●○◐`) and a 24-bit palette, which works in any
modern terminal without a patched font.

### Mouse

| | |
|---|---|
| click a tab | switch to it |
| click a row | select it · double-click toggles the detail pane |
| click a stack header | fold or unfold it |
| click a column header | sort by that column · click again to reverse |
| drag a pane border | resize the pane · double-click the border to reset it |
| wheel | scrolls whatever is under the pointer — the list, or the log pane |
| click in a pane | focus it |
| click a palette entry | run it |
| click a toast | dismiss it early |
| click outside a dialog | cancel it |

Capturing the mouse means your terminal stops handling click-drag selection itself. **Hold
Shift while selecting** and every mainstream terminal bypasses the capture, so copy/paste keeps
working. If you'd rather not deal with it, `--no-mouse` turns the whole thing off and dockyard
never asks the terminal to report anything.

### Keys

Press `?` in the app for the authoritative list — it, the footer and the command palette are
all generated from the same table, so they can't drift.

| | |
|---|---|
| `1`–`4`, `tab` | switch tab |
| `↑`/`↓`, `j`/`k`, `g`/`G`, `^u`/`^d` | move |
| `⏎` | detail pane · `l` log pane · `^w` switch pane focus |
| `f` `w` `t` | logs: follow · wrap · timestamps |
| `u` `S` `r` `p` `K` | start · stop · restart · pause · kill |
| `s` | shell into the container |
| `d` `P` | delete selected · prune unused (both ask first) |
| `y` | copy id to the clipboard (OSC 52, works over SSH) |
| `/` `:` `?` | filter · command palette · help |
| `z` `space` `Z` | group by stack · fold one · fold all |
| `o` `O` `a` | sort column · reverse · show/hide stopped |
| `^r` `q` | refresh now · quit |

## Design notes

**One owner, no locks.** The main loop owns all application state outright. Background tasks —
the poller, the event subscription, one stats stream per running container, the log stream,
and each in-flight mutation — only ever send messages down a single channel. Nothing is shared,
so nothing is locked, and the render path can't block on the daemon.

**Streams are reconciled, not accumulated.** The stats manager diffs the running-container set
on every refresh and starts or aborts streams to match. The log stream carries a generation
number so lines still in flight when you move the selection are discarded rather than landing
in the wrong buffer, and attaching is debounced so holding `↓` doesn't open a stream per row.

**Stacks are read, not inferred.** A container's name usually starts with its compose project, and
grouping on that convention would be wrong: `container_name:` overrides it, and the project name
defaults to the directory the compose file happens to sit in. So grouping reads
`com.docker.compose.project` (falling back to `com.docker.stack.namespace`, which is what
`docker stack deploy` writes instead), and those labels are in the filter's search key too. The
payoff shows up when several stacks run the same service: filtering for `postgres` with grouping on
lists one under `argilla` and another under `lakefs` rather than four rows you have to tell apart by
their image tag. Grouping happens *after* filtering and sorting and only rearranges rows within a
stack, so neither is undone — but the headers themselves stay in name order, because a stack has no
single cpu figure and headers that reshuffled as usage drifted would be impossible to keep your
place in.

**The mouse hit-tests against the last frame.** A TUI has no widget tree to ask "what's at
these coordinates?", so the render pass records the rectangles it drew — tab extents, the table
body and its viewport offset, sortable column headers, palette rows, dialog buttons, pane
boundaries — and the mouse reducer consults that map. It's cleared and rebuilt every frame, so a
hit region can never outlive the thing that drew it. Scroll deliberately targets what's under
the pointer rather than what has keyboard focus, since that's what makes a wheel feel right.

**Dragged pane sizes are a preference, not a layout.** A splitter drag records one number; the
layout still owns the arithmetic, clamping that number against the current terminal on every
frame and writing the clamped value back. So a pane dragged taller than a later, smaller
terminal can hold gives way instead of squeezing its neighbour out, and doesn't spring back to
an unusable size when the window grows again.

**The terminal is always given back.** Raw mode and the alternate screen are unwound on clean
exit, on error, and from a panic hook. The exec handoff is the delicate case: the stdin reader
polls with a timeout rather than blocking in `read(2)`, so it can't swallow the keystroke you
type after the shell exits, and the return path avoids ratatui's `Terminal::clear()` because it
round-trips a cursor-position query that isn't reliable right after a container shell has been
issuing its own. Mouse reporting is dropped for the duration too — whatever you run inside the
container should decide that for itself — and restored on the way back.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --check
```

The unit tests cover the parts worth pinning down in isolation: the `docker stats` CPU and
memory arithmetic, log timestamp parsing, unicode-safe truncation, and the fuzzy matcher's
ranking. Everything else is verified by driving the real binary against a real daemon.

## Not included

Image pull/build, container create/run, acting on a whole stack at once, swarm service management,
and registry authentication. The command table and tab structure leave room for them.
