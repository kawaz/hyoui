# hyoui

> English | [日本語](./README-ja.md)

**hyoui** `/ˈhjoʊi/` (from Japanese 憑依, "spirit possession") — a transparent PTY
companion that "possesses" a child process and moves as one with it.

A wrapper that runs an arbitrary command inside a PTY. It stays transparent and
does nothing by default, but by "possessing" the child's I/O it gives you a
foothold to observe, rewrite, and externally control it.

## What it does (design)

- `hyoui -- cmd [args...]` — run an arbitrary command inside a PTY (argv resolved
  directly via execvp)
- **interactive mode** (default) — raw the real tty and act as a transparent proxy
- **headless mode** — run without a real tty. Give a virtual screen size with
  `--size COLSxROWS`, and pipe stdin into the child as in `cat input | hyoui -- cmd`
- Inject input into the PTY from outside via a Unix socket
- Stop conditions: `--timeout` / `--idle-timeout` / `--until <pattern>`
- Transparent bg/fg control: child-suspend and parent-suspend each follow
  configurable, mirrored behavior

## About the name

*Hyoui* (憑依) means "spirit possession" — something inhabits a host and becomes
one with it; the host looks ordinary yet can be moved from within. It captures
this tool's character: it accompanies a child process, lives and dies together
with it, and in headless mode becomes a control handle for outside forces.

## Status

PoC (a small personal tool). Design and implementation are in progress.

## License

MIT License — Yoshiaki Kawazu (@kawaz)
