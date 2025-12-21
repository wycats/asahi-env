# muvm + timeout + controlling TTY: job-control stop ("do_signal_stop")

Status: canonical

Date: 2025-12-19

## Executive summary

We have a minimal, evidence-backed reproducer where running `muvm` under GNU coreutils `timeout` **with inherited interactive TTY** causes the VM process (`comm=VM:fedora`) to end up **job-control stopped** (`State: T (stopped)`, `wchan=do_signal_stop`).

Key observations:

- The failing mode is strongly tied to **terminal job control / foreground process group (pgrp) semantics**, not a syscall deadlock.
- `timeout --foreground` is a reliable mitigation: it makes `timeout muvm true` complete in the same environment.
- Running the same command under a **PTY** (harness-controlled) tends to avoid this stop regime entirely.

This is related to, but not the same as, muvm issue #123 ("hangs on exit when not attached to a tty"). Our reproducer is about **having a TTY, but being in a background process group relative to it**.

Reference: https://github.com/AsahiLinux/muvm/issues/123

## Environment

- Host OS: Fedora Asahi Remix (aarch64)
- `muvm` on host, running a Fedora guest (`VM:fedora`)
- `timeout`: GNU coreutils `timeout`
- Evidence harness: `tools/edge-muvm-experiment` (Rust)

## Minimal repro (shell)

From an interactive shell attached to a TTY:

- Failing (often / reproducibly in harness):
  - `timeout 5s muvm true`
- Mitigation:
  - `timeout --foreground 5s muvm true`

In our experiments, `timeout 5s muvm true` returns `124` after the timeout elapses, while `timeout --foreground 5s muvm true` returns `0` quickly.

## Evidence / what we see when it fails

When the failing case is captured right before timeout, the VM process shows:

- `/proc/<pid>/status`: `State: T (stopped)`
- `/proc/<pid>/wchan`: `do_signal_stop`

The harness also captures job-control fields from `/proc/<pid>/stat`:

- `tty_nr` (controlling tty)
- `tpgid` (foreground process group of the tty)
- `pgrp` (process group of the stopped process)

and emits a compact compare section:

- `job_control_compare`: compares the VM process to its parent wrapper (`timeout`) and also resolves `tpgid` to the owning process (`comm` + `cmdline`).

A representative captured state (wrapped lines):

- VM process (`comm=VM:fedora`) and `timeout` parent are both `fg=no`.
- `tty_foreground_owner` is the harness/shell process group, not the wrapper.

The parent `timeout` process shows:

- `SigIgn` includes `SIGTTIN` and `SIGTTOU` (and `SIGPIPE`)

which is consistent with `timeout` explicitly managing job-control-related signals.

## Matrix runner (repro made deterministic)

The harness includes a 2×(2+1) matrix runner:

- stdio: `pty` vs `inherit tty`
- kill mechanism:
  - internal watchdog
  - `timeout <secs> ...`
  - `timeout --foreground <secs> ...`

Command:

- `./target/debug/edge-muvm-experiment --mode muvm-true-matrix --timeout 5 --matrix-runs 1`

Typical results:

- `pty/*`: exits `0` quickly
- `tty/internal`: exits `0` quickly
- `tty/timeout`: exits `124` with `stuck.txt` captured
- `tty/timeout-foreground`: exits `0` quickly

Artifacts are written under `.local/edge-muvm/muvm-true-matrix-<stamp>/` (not committed).

## Hypothesis / likely root cause (most conservative statement)

This looks like a classic terminal job-control scenario:

- The `timeout` wrapper and its child (`muvm` / `VM:*`) are in a process group that is **not** the tty foreground process group (`pgrp != tpgid`).
- Some terminal I/O or terminal-control operation from that background group triggers a stop (job-control stop), which matches `State: T (stopped)` and `do_signal_stop`.
- `timeout --foreground` changes the process-group/foreground handling enough to avoid the stop.

This is consistent with the observed evidence; it avoids asserting which syscall triggers it.

## Workarounds

- Prefer `timeout --foreground` when using `timeout` with `muvm` from an interactive terminal.
- Use a PTY wrapper when automation requires pseudo-terminal semantics.

## Open questions (for later)

- Why does `timeout` sometimes end up in a non-foreground process group (`pgrp != tpgid`) when launched from an interactive shell/harness? (Is it something about how we spawn it, or is it an interaction with `muvm`/children changing pgrps?)
- What exact terminal operation triggers the stop? (Background read  `SIGTTIN`, background write with `TOSTOP`  `SIGTTOU`, or an ioctl path that yields `SIGTTOU`?)
- Does `muvm` or the VM process call `setsid`/`setpgid` (or otherwise manipulate the controlling terminal) in a way that makes it fragile under wrapper tools?
- Is there a preferred/recommended wrapper pattern for `muvm` (docs), or should `muvm` be robust to being started from a non-foreground pgrp when attached to a tty?

## Proposed upstream report

### Title

`muvm` can end up job-control stopped (`do_signal_stop`) when run under `timeout` with inherited TTY; `timeout --foreground` avoids it

### Body (copy/paste)

Environment:

- Host: Fedora Asahi Remix (aarch64)
- muvm version: (fill in)
- coreutils `timeout` version: (fill in)

Repro:

1. Open an interactive terminal.
2. Run: `timeout 5s muvm true`
3. Observe: command does not complete; after 5s `timeout` exits with code 124.
4. Run: `timeout --foreground 5s muvm true`
5. Observe: completes quickly with exit code 0.

Observed when failing:

- The `VM:*` process ends up `State: T (stopped)` and `wchan=do_signal_stop` (job-control stop path).
- `pgrp != tpgid` for the wrapper/VM process group; terminal foreground `tpgid` belongs to the invoking shell/harness.
- The wrapper `timeout` ignores `SIGTTIN`/`SIGTTOU`.

This suggests a terminal foreground-process-group / job-control interaction rather than a normal deadlock.

Evidence:

- I can attach an artifact directory produced by a deterministic matrix runner (below) that captures `/proc/<pid>/status`, `/proc/<pid>/stat` (pgrp/tpgid/tty_nr), and a side-by-side compare of the VM process and the `timeout` parent.

Matrix harness (optional):

- `edge-muvm-experiment --mode muvm-true-matrix --timeout 5 --matrix-runs 1`

Questions:

- Does `muvm` (or its VM process) call `setpgid`/`setsid` in a way that interacts poorly with wrapper tools?
- Is there recommended guidance for wrappers like `timeout`, or should muvm be robust to being started in a non-foreground pgrp when attached to a tty?

### Notes / related issues

- Related but distinct: #123 is about running without a tty; this report is about inheriting a tty but being in the background pgrp.

## Local pointers

- Harness code that generates these snapshots lives in `tools/edge-muvm-experiment`.
- The snapshot format includes:
  - `status_signals_decoded`
  - `job_control`
  - `job_control_compare` (includes `tty_foreground_owner`)
