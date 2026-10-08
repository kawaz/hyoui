# `hyoui run --pty-stdin`

**In one line**: connect the child's stdin to hyoui's PTY as well, and do not use the caller's stdin.

The specification is [DR-0042](../decisions/DR-0042-non-tty-stdin-is-the-childs-fd.md) decision 3. This document explains how to use it.

## What the child's stdin becomes

hyoui always creates a fresh PTY when it starts a child. The child's stdout / stderr and its controlling terminal are always that PTY. Only the child's stdin differs.

| How it is started | The child's stdin |
|---|---|
| `hyoui run -- cmd` from a terminal | the PTY |
| `printf … \| hyoui run -- cmd` (stdin is not a terminal) | the caller's pipe, passed as is — the same wiring as a shell's `\|` |
| `hyoui run --pty-stdin -- cmd` | the PTY, whatever the caller's stdin is |

## When to use it

Use it when a launcher without a terminal (an agent's Bash tool, the web gateway, a script) starts a shell or REPL that is driven from outside afterwards.

```sh
SESS=$(hyoui run --detached --pty-stdin -- bash -i)
hyoui input "$SESS" 'text:ls' key:Enter
hyoui screen dump "$SESS"
```

Without it, the child's stdin is the caller's `/dev/null` (or a socket). `bash -i` reads EOF from it and exits right away. Even if it did not exit, bash reads fd 0, so `hyoui input`, which writes to the PTY, would not reach it. It is the same as `docker run … bash` exiting at once without `-it`.

Programs that read stdin, such as a python REPL or `cat`, need `--pty-stdin` for the same reason.

## When not to use it

TUIs such as claude / vim / fzf / less read keys from `/dev/tty` (= hyoui's PTY) whatever stdin is. Keys from `hyoui input` and attach reach them without the flag.

Do not use it when you want to hand the first input through a pipe. With the flag the pipe is not used.

```sh
# sent as the first prompt (without the flag)
printf 'get ready and wait' | hyoui run --detached -- claude
```

## How keys reach the child with the flag

The caller's stdin flows neither to the child nor to the PTY. Only these two reach the child:

- `hyoui input` (from anywhere)
- attach (keystrokes when the caller has a terminal)

To feed a file's content into the PTY as input, use `hyoui input <id> file:<path>`.

## Background: fds and the controlling terminal

A process has two kinds of paths. fds 0 / 1 are the horizontal flow through which programs hand data to each other; `|` and `<` / `>` rewire them. The controlling terminal is the vertical axis to the person or controller: keys, the screen and the `^C` signal travel along it. Opening `/dev/tty` opens the controlling terminal wherever fd 0 points.

hyoui holds only the vertical axis (the PTY) and passes the caller's horizontal wiring to the child the way a shell does. `--pty-stdin` moves that horizontal entry (fd 0) onto the vertical axis too. See [DESIGN, "The child's fds and controlling terminal"](../DESIGN.md) for the diagram and details.
