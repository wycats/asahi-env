# Edge via muvm: pthread_create(EAGAIN) + empty --dump-dom

Status: canonical

Date: 2025-12-20

## Goal

Produce a reproducible, evidence-backed understanding of why Microsoft Edge (x86_64 via FEX) under `muvm` can exhibit:

- nondeterministic headless failures (e.g. empty `--dump-dom`), and/or
- Chromium logs like `pthread_create: Resource temporarily unavailable (11)`.

This document is intended to be the **single source of truth for this investigation**.

## Constraints

- No downloads in-repo. The Edge RPM is provided manually.
- Keep artifacts contained under `.local/edge-muvm/` (not committed).
- Prefer workflows encoded in repo tooling (Rust) over ad-hoc shell pipelines.

## Current facts (high confidence)

### F0: `timeout muvm true` can wedge due to job-control / controlling TTY

- When `muvm` is run under GNU `timeout` with an inherited interactive TTY, the VM process can end up job-control stopped:
  - `State: T (stopped)` and `wchan=do_signal_stop`.
- This is not the same problem as Edge `pthread_create` issues; it is a **separate confounder** that can make automation look like a hang.
- Mitigations:
  - Prefer `timeout --foreground …` when a controlling TTY is involved.
  - Prefer running `muvm` under a PTY wrapper for automation.

Canonical details and evidence: `docs/agent-context/research/muvm-timeout-tty-job-control.md`.

### F1: We can capture evidence for Edge runs in a consistent artifact format

The Rust harness `tools/edge-muvm-experiment` produces per-run directories (under `.local/edge-muvm/`) containing:

- `stdout.txt`, `stderr.txt`, `stderr.filtered.txt`
- `preflight.txt` (guest-side snapshot of limits/cgroups + basic state)
- `ps.txt`, `threads.txt`
- `edge-exit.txt`
- `summary.txt` (machine-readable key/value-ish report)
- Optional: `strace.<id>` files when `--strace` is enabled
- `pthread.stack-mprotect-enomem.txt` (T1 classifier report; even if 0 events)

## Working hypotheses (explicit)

### H1 (working): Some `pthread_create(EAGAIN)` reports correspond to stack setup failure

- In glibc/pthreads, `pthread_create` may surface `EAGAIN` when thread stack setup fails.
- A candidate “fatal” signature is:
  - `mmap(... MAP_STACK ...) = <addr>`
  - followed later (nearby) by: `mprotect(<addr>, <size>, PROT_READ|PROT_WRITE) = -1 ENOMEM`

This is a **working hypothesis**.

Evidence status in this workspace:

- We have captured runs with `pthread_create` lines.
- We have captured at least one T1-positive run (MAP_STACK → `mprotect(...)=ENOMEM`), and updated the classifier to match the observed guard-page pattern (`mprotect` at `mmap_base + 0x1000`).
  - Example: `.local/edge-muvm/headless-1766268125` re-analysis shows `analysis_events_total: 2`.

## Canonical workflows

### 1) Single headless run (artifact capture)

- `cargo run -p edge-muvm-experiment -- --mode edge --timeout 90`

Optional knobs (recently added):

- Place the profile directory inside the guest (avoids shared/virtio-fs profile I/O):
  - `cargo run -p edge-muvm-experiment -- --mode edge --timeout 90 --profile-location guest-tmp`
- Pass extra Edge flags (repeatable):

  - `cargo run -p edge-muvm-experiment -- --mode edge --timeout 90 --edge-arg=--no-sandbox`

- Set environment variables for the Edge process (repeatable):

  - `cargo run -p edge-muvm-experiment -- --mode edge --timeout 90 --edge-env=CHROME_HEADLESS=1`

- Preserve DBus/XDG env vars when invoking `muvm` (disables the harness’s default clearing of `DBUS_SESSION_BUS_ADDRESS` and `XDG_RUNTIME_DIR`):

  - `cargo run -p edge-muvm-experiment -- --mode edge --timeout 90 --preserve-dbus-xdg-env`

- Best-effort guest sysctl writes (repeatable; logs to `guest-sysctl.txt`):

  - `cargo run -p edge-muvm-experiment -- --mode edge --timeout 90 --guest-sysctl=vm.overcommit_memory=1`

- Pass `muvm --privileged` (intended to run the command as root inside the VM; see evidence below about current behavior on this system):
  - `cargo run -p edge-muvm-experiment -- --mode edge --timeout 90 --muvm-privileged`

Notes:

- The guest-runner enforces an additional per-Edge watchdog (`--edge-watchdog-seconds`, default 45) to keep runs bounded.

Optional tracing:

- `cargo run -p edge-muvm-experiment -- --mode edge --timeout 90 --strace`

### 2) Repeat until a condition (no shell loops)

Repeat runs until the first instance of a chosen symptom:

- Stop on any `pthread_create` line:

  - `cargo run -p edge-muvm-experiment -- --mode edge-repeat --timeout 90 --strace --repeat-stop-on pthread-create --repeat-max-attempts 12`

- Stop on the T1 signature (preferred when hunting H1):
  - `cargo run -p edge-muvm-experiment -- --mode edge-repeat --timeout 90 --strace --repeat-stop-on stack-mprotect-enomem --repeat-max-attempts 30`

### 3) Re-analyze an existing run dir

- `cargo run -p edge-muvm-experiment -- --mode analyze-run-dir --run-dir .local/edge-muvm/headless-1766268125`

### 3) Minimal job-control reproduction (separate confounder)

- `cargo run -p edge-muvm-experiment -- --mode muvm-true-matrix --timeout 5 --matrix-runs 1`

## Artifact contract (how to interpret results)

### `summary.txt`

Key fields:

- `stderr_pthread_create_lines`: count of lines matching `pthread_create`
- `pthread_ids_from_stderr`: space-separated `pid:tid` extracted from Chromium-style `[pid:tid:…]` prefixes
- `pthread_stack_mprotect_enomem_events`: total events found by the T1 classifier

### `pthread.stack-mprotect-enomem.txt`

- For each `pid:tid` observed in `stderr`, the classifier:
  - prefers `strace.<tid>` when present
  - falls back to `strace.<pid>`
  - also supports `host.strace.<id>` if host-side tracing is used

Interpretation:

- `stack_mprotect_enomem_events_total: 0` means “we did not observe the MAP_STACK→mprotect(ENOMEM) signature for those ids in available strace files”.
- It does **not** prove `pthread_create` wasn’t caused by some other resource constraint.

## Scientific plan (big-picture, systematic)

This is the plan we should follow to avoid “local hill climbing”. It is designed to be:

- falsifiable (each hypothesis has a test)
- controlled (one variable per experiment)
- grounded (confounders are explicitly excluded)

### Evidence inventory (what is true right now)

- Goal: explain + stabilize Edge under `muvm` (x86_64 via FEX), including failures like empty `--dump-dom` and/or `pthread_create(...)=EAGAIN`.
- Confirmed confounder: `timeout muvm true` with an inherited controlling TTY can yield a job-control stop (`State: T (stopped)`, `wchan=do_signal_stop`). This can masquerade as “hang”. Avoid it (PTY wrapper and/or `timeout --foreground`).
- Harness contract is stable: `tools/edge-muvm-experiment` produces run dirs with `summary.txt`, raw logs, and optional `strace.<id>`.
- Current observed outcomes in this workspace:
  - We can capture runs that emit `pthread_create` lines.
  - We have captured at least one T1-positive run (`stack_mprotect_enomem_events_total > 0`) after updating the classifier to match the observed syscall sequence.
  - Many headless runs show `stdout_bytes: 0` and `edge_exit: signal 9 (SIGKILL)` (bounded by the guest-side watchdog), so “empty `--dump-dom`” may often mean “did not reach completion before kill”.

New evidence (T0 control: `data:` URL):

- 2025-12-20: `data:text/html,<title>ok</title><h1>ok</h1>` did **not** produce non-empty `--dump-dom` stdout in either of these batches:
  - Batch A: 10 attempts, `--timeout 90`, guest watchdog 45s → no hit (`.local/edge-muvm/edge-repeat-1766262099.txt`)
  - Batch B: 3 attempts, `--timeout 240`, guest watchdog 180s → no hit (`.local/edge-muvm/edge-repeat-1766262719.txt`)
- Representative run: `.local/edge-muvm/headless-1766262719/summary.txt`:
  - `stdout_bytes: 0`
  - `edge_exit: signal: 9 (SIGKILL)` after ~180s
  - `stderr_pthread_create_lines: 611`
  - stuck snapshot (`stuck.txt`) shows msedge leader sleeping in `do_poll.constprop.0` (not job-control-stopped).

Interpretation:

- This strongly supports treating “non-completion + watchdog kill” as the primary symptom to explain (H0), and it also weakens “purely network/I/O” explanations for empty output (since `data:` removes DNS/TLS).
- Next step should prioritize T3 (extract the real errno chain for `pthread_create`) with `--strace`, rather than expanding URL-class tests.

New evidence (T0 extension: “make non-completion actionable” via stuck snapshot decode):

- 2025-12-20: With the upgraded `stuck.txt` snapshot (captures `/proc/<pid>/syscall` and decodes `ppoll` fd targets), a 3-run batch with `--strace` and a 60s guest watchdog still produced **no completion**, but it did produce a consistent “blocked-on” signature:
  - Command shape (per run):
    - `cargo run -p edge-muvm-experiment -- --mode edge --mem 2048 --strace --timeout 120 --edge-watchdog-seconds 60 --url 'data:text/html,<title>ok</title><h1>ok</h1>'`
  - Run dirs:
    - `.local/edge-muvm/headless-1766271555`
    - `.local/edge-muvm/headless-1766271616`
    - `.local/edge-muvm/headless-1766271678`
  - Example signature (`.local/edge-muvm/headless-1766271555/stuck.txt`):
    - msedge leader (tid=270) sleeping in `do_poll.constprop.0` with a decoded `ppoll(nfds=2)` waiting on:
      - `anon_inode:[eventfd]` and a **self-pipe** (`pipe:[1116]`), with the wait graph indicating the pipe endpoints are owned by msedge itself.
    - `sandbox_ipc_thr` (tid=284) also in `ppoll(nfds=2)` waiting on:
      - `pipe:[5133]` and an unnamed unix socket (`socket:[5132]`), which appears to be an internal socketpair (no path).

Additional evidence (longer run without `--strace`):

- 2025-12-20: Increasing the watchdog significantly still did not yield completion:
  - `cargo run -p edge-muvm-experiment -- --mode edge --mem 2048 --timeout 300 --edge-watchdog-seconds 240 --url 'data:text/html,<title>ok</title><h1>ok</h1>'`
  - Run dir: `.local/edge-muvm/headless-1766271888`
  - Result: still `edge_exit: signal: 9 (SIGKILL)` after ~240s and `stdout_bytes: 0`.
  - `stuck.txt` shows a “normal-looking” thread zoo (many threads parked in `ep_poll`/`futex_wait_queue`, IPC thread polling pipe+socket, inotify thread polling inotify), but no obvious external network wait.

Notes / limitations:

- `/proc/<pid>/stack` and `/proc/<pid>/task/<tid>/stack` are `Permission denied` in these guests, so we currently rely on `wchan`, `/proc/<pid>/syscall`, and decoded `ppoll` fd targets rather than kernel stack traces.

Interpretation:

- These snapshots make it less likely that the dominant “non-completion” symptom is due to waiting on external network/DNS/TLS.
- Instead, the process often appears to be parked on internal synchronization primitives (eventfd, self-pipes, unnamed unix sockets), which is compatible with an internal deadlock / missed wakeup / IPC stall hypothesis (H5-ish) even when the URL is `data:`.

New evidence (T2 partial: `--mem` sweep with `data:` URL, `--strace`):

- 2025-12-20: We ran a small sweep varying only muvm guest RAM while holding URL/headless impl/watchdogs constant:
  - Command shape (per run): `--mode edge --strace --timeout 120 --edge-watchdog-seconds 60 --url 'data:text/html,<title>ok</title><h1>ok</h1>' --mem <MiB>`
  - Runs were 3× each for `--mem` = 2048/3072/4096/6144 MiB.

Results summary:

| mem (MiB) | runs | T1-positive runs (events>0) | T1 events total | stdout non-empty |
| --------: | ---: | --------------------------: | --------------: | ---------------: |
|      2048 |    3 |                           1 |               2 |                0 |
|      3072 |    3 |                           0 |               0 |                0 |
|      4096 |    3 |                           1 |               2 |                0 |
|      6144 |    3 |                           1 |               2 |                0 |

Representative run dirs:

- T1-positive examples:
  - `.local/edge-muvm/headless-1766269441` (`--mem 2048`)
  - `.local/edge-muvm/headless-1766269807` (`--mem 4096`)
  - `.local/edge-muvm/headless-1766270051` (`--mem 6144`)

Interpretation:

- Completion: none of the 12 runs produced non-empty `--dump-dom` stdout (all were watchdog-killed at ~60s), so increasing guest RAM alone did not address the dominant “non-completion” symptom.
- T1 signature: the MAP_STACK→mprotect(ENOMEM) signature occurs intermittently across multiple memory sizes (not monotonic in this small sample).
- Commit accounting: in a T1-positive 2GiB run, `Committed_AS` was ~30MiB and `CommitLimit` ~1GiB, so the ENOMEM does not look like a simple “global commit limit exhausted” scenario at the moment preflight was captured.

New evidence (T2 support: capture guest overcommit / map-count policy):

- We now capture relevant guest sysctls in `preflight.txt` and surface them in `summary.txt` under `preflight_kvs`:
  - `vm_overcommit_memory`
  - `vm_overcommit_ratio`
  - `vm_overcommit_kbytes`
  - `vm_max_map_count`
- Example (2GiB run `.local/edge-muvm/headless-1766270675`):
  - `vm_overcommit_memory: 0`
  - `vm_overcommit_ratio: 50`
  - `vm_overcommit_kbytes: 0`
  - `vm_max_map_count: 65530`

Interpretation:

- The guest appears to be using heuristic overcommit (`0`) with a 50% ratio and no swap in our runs, which matches the observed `CommitLimit` being about half of `MemTotal`.
- This does not yet explain why we see intermittent `mprotect(...)=ENOMEM` at low apparent `Committed_AS`, but it anchors the environment so we can reason about commit policy without guessing.

New evidence (consultant hypotheses A/B: profile location, `--no-sandbox`, and map-count pressure):

- 2025-12-20: We added two harness knobs to test fast hypotheses without rewriting the runner:
  - `--profile-location guest-tmp` (profile under `/tmp` inside guest)
  - `--edge-arg=...` (repeatable passthrough to Edge)
  - We also began recording `/proc/<pid>/maps` line count (`maps_lines`) in `stuck.txt` at kill time.

Experiment:

- URL: `data:text/html,ok`
- `--edge-watchdog-seconds 60` and `--timeout 180`
- 3 trials each:
  - A: baseline (`PROFILE_LOCATION=shared`)
  - B: guest-local profile (`PROFILE_LOCATION=guest-tmp`)
  - C: guest-local + `EDGE_ARGS=--no-sandbox`

Run dirs:

- A (shared):
  - `.local/edge-muvm/headless-1766273341`
  - `.local/edge-muvm/headless-1766273402`
  - `.local/edge-muvm/headless-1766273463`
- B (guest-tmp):
  - `.local/edge-muvm/headless-1766273528`
  - `.local/edge-muvm/headless-1766273590`
  - `.local/edge-muvm/headless-1766273651`
- C (guest-tmp + no-sandbox):
  - `.local/edge-muvm/headless-1766273801581`
  - `.local/edge-muvm/headless-1766273863721`
  - `.local/edge-muvm/headless-1766273924868`

Results (high-level):

- Completion: 0/9 (all `stdout_bytes: 0` and `edge_exit: signal: 9 (SIGKILL)` after ~60s).
- `maps_lines`: ~1190–1268 across all runs, with `vm_max_map_count: 65530`.

Interpretation:

- This small A/B does not support “shared profile on virtio-fs” or “sandbox” as the primary cause of the `data:` non-completion symptom.
- It also makes the `vm.max_map_count` hypothesis look unlikely for this failure mode, since the process is nowhere near the ceiling at kill time.

Notes:

- Early during implementation, `--edge-arg --no-sandbox` could be mis-parsed because the value begins with `-`; the harness now forwards edge args as `--edge-arg=<value>` and accepts hyphen values.

New evidence (`--strace` A/B, `data:` URL):

- 2025-12-20: Ran 1× each with `--strace` against `data:text/html,ok` and the same 60s guest watchdog:
  - Shared: `.local/edge-muvm/headless-1766274123969`
  - Guest-tmp: `.local/edge-muvm/headless-1766274185050`
  - Guest-tmp + `--no-sandbox`: `.local/edge-muvm/headless-1766274246404`

Results:

- Completion: 0/3 (all watchdog-killed with `stdout_bytes: 0`).
- One run hit the T1 signature again (`pthread_stack_mprotect_enomem_events: 2`) in the guest-tmp condition:
  - In `.local/edge-muvm/headless-1766274185050/strace.336` and `.local/edge-muvm/headless-1766274185050/strace.376`:
    - `mmap(NULL, 8392704, PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS|MAP_STACK, -1, 0) = 0x7fffdf79f000`
    - `mprotect(0x7fffdf7a0000, 8388608, PROT_READ|PROT_WRITE) = -1 ENOMEM`

Additional observation:

- In all three straced runs we see attempts to reserve very high fixed addresses fail:
  - e.g. `mmap(0x200000000000000, 4096, PROT_NONE, ... MAP_FIXED_NOREPLACE, ...) = -1 ENOMEM`
  - Consultant interpretation: this is very likely V8 probing for LA57 / 5-level paging (57-bit VA). On Apple Silicon / Asahi, userspace VA is typically limited to 48-bit, so this probe should fail and Chromium should fall back.
  - Working verdict: treat these specific ENOMEMs as expected feature-detection noise unless proven otherwise.

New evidence (“kitchen sink” follow-up, `data:` URL):

- 2025-12-20: Added harness knobs to (a) preserve DBus/XDG env into the guest-runner and (b) attempt guest sysctl writes.
- Ran a 1× matrix with a “kitchen sink” flag set:
  - URL: `data:text/html,ok`
  - `--profile-location guest-tmp`
  - `--edge-arg=--no-sandbox --edge-arg=--disable-software-rasterizer`
  - `--timeout 180 --edge-watchdog-seconds 60`
  - Cases:
    - A baseline: `.local/edge-muvm/headless-1766276452708`
    - B preserve env: `.local/edge-muvm/headless-1766276513943`
    - C sysctl attempt: `.local/edge-muvm/headless-1766276575224`
    - D preserve env + sysctl attempt: `.local/edge-muvm/headless-1766276636471`

Results:

- Completion: 0/4 (all watchdog-killed with `stdout_bytes: 0`, ~61s).
- In these runs, `stderr_pthread_create_lines: 0` (the “non-completion” symptom persists even without the high-volume `pthread_create` spam).
- DBus env preservation did not produce a usable session bus inside the guest:
  - Baseline clears `DBUS_SESSION_BUS_ADDRESS` to an empty string.
  - Preserve-env leaves it unset (`(unset)` in preflight).
  - In both cases Chromium still logs “Failed to connect to the bus…” lines (`stderr_dbus_lines` non-zero).
  - `XDG_RUNTIME_DIR` is set by muvm to `/tmp/muvm-run-1000-...` regardless.

Sysctl attempt results:

- `--guest-sysctl=vm.overcommit_memory=1` fails with `Permission denied` writing `/proc/sys/vm/overcommit_memory` (see `guest-sysctl.txt` in the run dirs above).

Attempted escalation via muvm:

- `muvm --privileged` currently still runs commands as uid 1000 on this machine:
  - `timeout --foreground 30s muvm --privileged id -u` prints `1000`.
  - Correspondingly, the harness’s `--muvm-privileged` does not make the guest sysctl writable.

Interpretation:

- The overcommit-policy hypothesis may still be correct, but we cannot currently test it by flipping guest sysctls (no effective privilege escalation inside muvm on this system).
- The “non-completion” symptom persists even when we eliminate `pthread_create` spam (suggesting there may be at least two regimes: one with loud thread creation failures, and another silent early stall).

New hypothesis (pivot): zygote/IPC hang under muvm+FEX

- Consultant interpretation: the kitchen-sink runs strongly suggest T1 (pthread/stack ENOMEM) is not the root cause of the dominant hang.
- New working model: Edge/Chromium is hanging in its zygote / multi-process initialization or IPC handshakes under emulation.

New evidence (zygote simplification attempts):

- Goal: bypass zygote + IPC complexity using Chromium’s “nuclear option” flags.
- URL: `data:text/html,ok`
- Profile: `--profile-location guest-tmp`
- Common flags: `--no-sandbox --disable-software-rasterizer`

Runs:

- `--no-zygote --single-process`:

  - `.local/edge-muvm/headless-1766278866472` (watchdog 180s): **early crash** with `edge_exit: signal: 5 (SIGTRAP)` and `stdout_bytes: 0`.
  - Repeat `.local/edge-muvm/headless-1766278885580` (watchdog 120s): same outcome (`SIGTRAP`).

- `--single-process` only:

  - `.local/edge-muvm/headless-1766279010142`: same early `SIGTRAP` with `stdout_bytes: 0`.

- `--no-zygote` only:

  - `.local/edge-muvm/headless-1766278888936` (watchdog 120s): no early crash, but still **non-completion** (`SIGKILL`, `stdout_bytes: 0`).

- Note: current Chromium/Edge requires `--no-zygote` to be paired with `--no-sandbox`.
  - `.local/edge-muvm/headless-1766279786544` (strace hang mode): immediate exit with stderr:
    - `Zygote cannot be disabled if sandbox is enabled. Use --no-zygote together with --no-sandbox`

Observations:

- The `SIGTRAP` appears correlated with `--single-process` (with or without `--no-zygote`).
- Stderr for `--single-process` runs includes repeated:
  - `ptrace: Operation not permitted (1)` and `Unexpected registers size ...` from crashpad.
  - `Cannot use V8 Proxy resolver in single process mode.`

Follow-up attempt:

- Tried `--no-zygote` plus extra crashpad-disabling flags (`--disable-breakpad --disable-crash-reporter --disable-features=Crashpad`):
  - `.local/edge-muvm/headless-1766279029465`: still non-completion (`SIGKILL`, `stdout_bytes: 0`), and crashpad `ptrace` errors still appear.

Interpretation:

- `--no-zygote` alone is not sufficient to make `--dump-dom` complete in this environment.
- `--single-process` is not usable here as-is due to an early `SIGTRAP` crash.
- Next: if we want to keep pursuing the zygote/IPC angle, we likely need strace-guided evidence for what the process is blocked on in the `--no-zygote` case, and/or find a flag combination that avoids `--single-process` while still eliminating the problematic IPC path.

New evidence (hang-focused `strace -ff` on `--no-zygote --no-sandbox`):

- `.local/edge-muvm/headless-1766279810410` (watchdog 30s, `--strace --strace-mode hang`): **non-completion** (`SIGKILL`, `stdout_bytes: 0`, no pthread errors).
- Stuck snapshot (main process pid 271) shows Edge blocked in `ppoll`:
  - `wchan=do_poll.constprop.0`, syscall 73 (`ppoll`) waiting on:
    - `fd=16` (eventfd)
    - `fd=17` (pipe inode 6869)
- Multiple child processes die via `SIGTRAP` during startup.
  - Example: `strace.361` and `strace.362` are both `--type=renderer` processes.
  - Pattern in each:
    - receives `SIGTRAP (TRAP_BRKPT)` at an address in the high (`0xaaaa...`) range,
    - sends a small `sendmsg` on fd 3,
    - waits for `SIGCONT` from pid 283,
    - then self-sends `SIGTRAP` and is killed.
  - `ps.txt` identifies pid 283 as `msedge_crashpad_handler`, suggesting this is Chromium’s crash/exception reporting handshake even though we passed crashpad-disabling flags.

Interpretation (tentative):

- The `--no-zygote --no-sandbox` path still doesn’t complete, but we now have a concrete failure signal: renderer processes are crashing (`SIGTRAP`) during early startup, and the browser process is blocked waiting on IPC/eventfds.

New evidence ("loud" logging + SIGTRAP address mapping):

- The harness now supports setting environment variables for the Edge process via repeatable `--edge-env=KEY=VALUE`.
- Run: `.local/edge-muvm/headless-1766280991676` (`--no-zygote --no-sandbox --enable-logging=stderr --v=1`, `--edge-env=GOOGLE_API_KEY=no`, `--edge-env=CHROME_HEADLESS=1`, `--edge-env=BREAKPAD_DUMP_LOCATION=/tmp`, plus `--strace --strace-mode hang`).
- Renderer crashes are reproducible and still occur very early:
  - `strace.361`: `+++ killed by SIGTRAP +++` with `si_addr=0xaaaaaf5603f4`.
  - `strace.362`: `+++ killed by SIGTRAP +++` with `si_addr=0xaaaae1b303f4`.
- Crucially, both `si_addr` values map into the address range used by `/usr/bin/FEXInterpreter` in the renderer’s own `/proc/self/maps` dump (captured via the renderer reading its maps in the trace):
  - Example from `strace.361` shows `/usr/bin/FEXInterpreter` mapped at `aaaaaf280000-aaaaaf55e000` (r-xp) and `aaaaaf56c000-...` (r--p), and the trap address `0xaaaaaf5603f4` is immediately adjacent to that region.

Interpretation (updated):

- We are very likely looking at a failure in FEX (or its code-cache / signal trampoline machinery) rather than a Chromium/V8 `CHECK()` printing a message to stderr.
- This would explain why earlier searches for `Check failed` / `FATAL:` in renderer stderr output came up empty: the trap happens in (or adjacent to) `/usr/bin/FEXInterpreter` mappings.
- Next step: treat the renderer `SIGTRAP` as an emulation/runtime fault and pursue evidence in that direction (e.g. correlate the `si_addr` mapping type, and compare behavior with and without crashpad involvement).

FEX-level logging experiments:

- Goal: try to get emulator-side diagnostics (unknown instruction, decode failure, etc.) as a complement to the renderer `SIGTRAP` address mapping evidence.
- Attempt 1 (simple logging env vars): `.local/edge-muvm/headless-1766284318640`
  - `--edge-env=FEX_LOGLEVEL=TRACE` / `FEX_OUTPUTLOG=stderr` / `FEX_SILENTLOG=0` (plus some redundant variants).
  - Result: no FEX-prefixed lines observed in `stderr.txt` (confirmed with `rg`), even though `preflight.txt` confirms the env vars were injected.
- Attempt 2 (force “fresh” server socket): `.local/edge-muvm/headless-1766285107555`
  - `--edge-env=FEX_SERVERSOCKETPATH=/tmp/fexserver-edge-muvm.sock` with `FEX_OUTPUTLOG=stderr` and `FEX_SILENTLOG=false`.
  - Result: still no FEX-prefixed lines in `stderr.txt`.
- Attempt 3 (force debug output): `.local/edge-muvm/headless-1766285199525`
  - `--edge-env=FEX_DUMPIR=stderr` and `--edge-env=FEX_DISASSEMBLE=dispatcher`.
  - Result: FEX *does* honor these options and begins emitting IR dumps to `stderr.txt` immediately.
  - Caveat: this produces an enormous stderr (`~1.5G`, `~43M` lines) and can easily overwhelm typical log-grep workflows.
- Attempt 4 (redirect IR dumps to shared folder): `.local/edge-muvm/headless-1766285431364`
  - `--edge-env=FEX_DUMPIR=/home/wycats/Code/Personal/asahi/.local/edge-muvm/fex-dumpir`.
  - Result: stderr stays small, but the dump directory grows quickly (observed `~78k` `*-post.ir` files totaling `~1.5G`).

Interpretation (tentative):

- “Normal” FEX logging toggles didn’t produce obvious messages in our environment, but FEX debug dumping knobs (`FEX_DUMPIR`, `FEX_DISASSEMBLE`) clearly apply and can be used to extract emulator-side state if we can find a way to bound the output.
- Next step here (if we pursue it): identify a *bounded* FEX debug setting that emits a small amount of information prior to the renderer `SIGTRAP` (or isolate to just the renderer via per-app config), rather than emitting every IR block.

FEX JIT/SMC hypothesis experiments:

- Motivation: AVX/AVX2 disabling at the Chromium layer did not prevent renderer `SIGTRAP` in FEX mappings, suggesting this is not “one unsupported instruction” but potentially JIT/SMC coherence.

- Experiment: Force FEX “interpreter mode” (attempt) via `FEX_CORE=0`
  - Run: `.local/edge-muvm/headless-1766288581818` (`--edge-watchdog-seconds 220`, `--edge-env=FEX_CORE=0`).
  - Result: still no completion (`stdout_bytes: 0`), guest watchdog kills Edge (`edge_exit: SIGKILL`).
  - Note: env injection confirmed by `preflight.txt` (`EDGE_ENV=FEX_CORE=0`), but this does not prove that FEX accepted/understood the value.

- Experiment: Force stricter SMC checks via `FEX_SMCCHECKS=1`
  - Run: `.local/edge-muvm/headless-1766288812620` (`--edge-watchdog-seconds 90`, `--edge-env=FEX_SMCCHECKS=1`).
  - Result: still no completion (`stdout_bytes: 0`), guest watchdog kills Edge (`edge_exit: SIGKILL`).
  - Follow-up with tracing: `.local/edge-muvm/headless-1766288914087` (`--strace --strace-mode hang`, `--edge-env=FEX_SMCCHECKS=1`).
    - Renderer subprocesses still die via `SIGTRAP` (examples: `si_addr=0xaaaab8bb03f4`, `0xaaaaba2203f4`, `0xaaaac54a03f4`).
    - Also observed multiple `SIGILL` events in other traced processes (ILL_ILLOPC) during the same run, which strengthens the case that we are observing emulation/JIT correctness issues rather than Chromium’s own intentional crash path.

Interpretation (tentative):

- Neither `FEX_CORE=0` nor `FEX_SMCCHECKS=1` altered the basic failure mode (renderer `SIGTRAP`, browser hang, watchdog kill).
- We may need the correct/actual FEX config values for these knobs (e.g. enum strings rather than numeric), or a different knob that disables the JIT / forces full SMC validation.

### First principles

- Separate symptoms from causes.
  - Symptoms: `pthread_create` lines, empty `stdout`, `SIGKILL`, long runtimes.
  - Causes: pids/tasks limits, memory policy, overcommit, emulation quirks, environment stalls, host/kernel interactions.
- Define “good” and “bad” runs operationally (from artifacts), not narratively.
- Confounders must be held constant or excluded (especially the job-control stop regime).
- One independent variable per experiment; measure rates (N>1), not anecdotes.

### Hypotheses (MECE)

- H0: Harness artifact: the harness kills Edge before it finishes dumping DOM; empty stdout is downstream of “never completed”.
- H1: Thread/task/pids limits: TasksMax, cgroup pids controller, RLIMIT_NPROC, or kernel thread limits cause real `pthread_create(EAGAIN)`.
- H2: Memory accounting/overcommit/cgroup memory: stack reservation/commit fails, producing `pthread_create(EAGAIN)` (not necessarily via the current T1 signature).
- H3: Signature mismatch / FEX-specific path: failure exists, but our classifier is too narrow (different syscall/errno chain than MAP_STACK→mprotect(ENOMEM)).
- H4: Guest environment/I/O stall: URL/network/DBus/sandbox conditions stall the process; `pthread_create` may be incidental or secondary.
- H5: Host/kernel/muvm/FEX interaction: version- or load-dependent host behavior causes stalls or resource exhaustion.

### Empirical tests (falsifiable)

Each test specifies a control, a perturbation, measurements, and a decision rule.

#### T0: Confirm the harness measures “completed vs killed” (tests H0)

- Control: `--mode edge` against a minimal page source (prefer `data:` URL) with `--timeout` high enough to allow completion.
- Perturbation: vary only the URL type (data URL vs external https), keeping mem/strace constant.
- Measurements:
  - completion proxy: `stdout_bytes > 0`
  - termination: `edge_exit` (natural exit vs SIGKILL)
  - elapsed time
- Decision:
  - If `data:` is reliably non-empty but https is not, the primary problem is environment/I/O (push toward H4).
  - If even `data:` fails to complete, treat “not completing” as the main problem to explain before deep classifier work.

#### T1: Task/thread limit sweep (tests H1)

- Control: `--mode edge-repeat --repeat-stop-on pthread-create` with constant `--mem`, `--timeout`, `--strace`.
- Perturbation: vary `--systemd-tasks-max` (small → large) while holding everything else fixed.
- Measurements:
  - `stderr_pthread_create_lines` rate
  - `preflight.txt`: `/proc/self/limits`, cgroup pids state (`pids.max`, `pids.current` when available)
- Decision:
  - If lowering TasksMax increases `pthread_create` frequency sharply, H1 is supported.
  - If no change across a wide sweep, H1 weakens.

#### T2: Memory/overcommit sweep (tests H2)

- Control: constant URL + strace enabled.
- Perturbation:
  - vary `--mem` (e.g. 2048/4096/6144)
  - separately vary guest overcommit policy (e.g. `vm.overcommit_memory`), one change at a time
- Measurements:
  - symptom rates (`pthread_create`, completion)
  - memory signals from `preflight.txt` (`/proc/meminfo`, cgroup memory files)
- Decision:
  - strong dependence on `--mem`/overcommit supports H2.

#### T3: Extract the actual errno path (tests H3)

If we continue seeing `pthread_create` without T1 events, treat the classifier as suspect.

- Control: runs that hit `pthread_create` under `--strace`.
- Perturbation: expand `strace -e trace=` set to include additional suspects (while keeping it bounded and diffable).
- Measurements:
  - identify the earliest consistent failing syscall/errno chain in the same pid/tid that logged `pthread_create`.
- Decision:
  - update the classifier to target the observed chain (do not insist on MAP_STACK→mprotect(ENOMEM)).

#### T4: Environment/I/O stall isolation (tests H4)

- Control: constant `--mem`, constant timeout, same headless impl.
- Perturbation: URL classes (data URL / simple http / external https) one at a time.
- Measurements:
  - completion rate (`stdout_bytes`)
  - stderr indicators (SSL/handshake/DNS, DBus)
- Decision:
  - if failures cluster by URL class, prioritize environment/IO work over resource-limit hunting.

#### T5: Host/kernel/muvm/FEX contribution (tests H5)

- Control: fixed experiment parameters, repeated runs.
- Perturbation: change only one of: host kernel, muvm version, FEX version, or host load conditions.
- Measurements: symptom rates + artifact metadata.
- Decision: rate shifts across versions imply an upstream-worthy minimal repro.

### Stop conditions (anti-local-hill)

- If T0 indicates we are mostly killing before completion, stop “pthread classifier” work and make completion unambiguous.
- If T1/T2 shows a strong correlation, narrow to a minimal reproducible case with that variable as the independent axis.
- If no correlations appear, switch to T3: extract the real errno path rather than forcing the T1 narrative.

## Next experiments (falsifiable)

- T2: Overcommit policy flip (guest)

  - Flip `vm.overcommit_memory` and compare:
    - rate of `pthread_create` errors
    - rate of T1 events

- T3: Limits + cgroups snapshot correlation

  - Use the already-captured `preflight.txt` to correlate:
    - `/proc/self/limits`
    - cgroup membership and pids/memory controller state

- T4: Repeatability matrix per `--mem`
  - Run multiple trials per `--mem` setting and compare symptom rates.

## Deletion / promotion criteria

- If this investigation converges to stable operational guidance (“always do X/Y”), promote that guidance into `docs/manual/…` and leave this doc as either:
  - a pointer stub, or
  - an evidence appendix.

## Prior art / expert priors (to sharpen hypotheses)

These are “known patterns” from pthreads/glibc/Linux + community reports that help interpret `pthread_create: Resource temporarily unavailable (11)`.

### P1: `pthread_create` error codes are lossy summaries

- POSIX requires `pthread_create` to return `EAGAIN` for “insufficient resources to create another thread”. In practice, libc implementations sometimes translate lower-level failures into `EAGAIN`, so `EAGAIN` does not uniquely imply “process/thread count limit”.
- glibc history/prior art: there are long-lived discussions/bugs around whether the underlying cause is ENOMEM vs EAGAIN and how it should be surfaced; the practical takeaway is: treat `EAGAIN` as “resource exhaustion somewhere in the stack”, not a single cause.

Implication for this investigation:

- H1 (task/pids limits) and H2 (memory/overcommit) can both present as `pthread_create(EAGAIN)`.

### P2: Common Linux causes of `pthread_create(EAGAIN)`

The most common root causes (worth explicitly checking/correlating in `preflight.txt`):

- **pids/tasks limits**: cgroup `pids.max`, systemd `TasksMax`, or a container-style pids controller (often manifests as `clone(...) = -1 EAGAIN`).
- **user/process limits**: `RLIMIT_NPROC` (aka `ulimit -u`) or per-user process/thread quotas.
- **kernel global limits**: `/proc/sys/kernel/threads-max`, `/proc/sys/kernel/pid_max` (rare but worth ruling out if the guest is constrained).
- **address space / commit / overcommit**: inability to reserve/commit a thread stack or guard pages (may manifest as `mmap`/`mprotect` returning ENOMEM/EPERM depending on the failure).

Implication:

- The right next step after seeing `pthread_create` is usually “find the first failing syscall/errno in the same pid/tid”, not “assume thread limit”. This maps directly to T3.

### P3: Chromium/containers/systemd anecdotal reports (pattern match)

Across Chromium-like workloads in constrained environments, “pthread_create failed: Resource temporarily unavailable” is frequently correlated with task limits (systemd/cgroups) rather than classic “out of RAM” scenarios.

Implication:

- T1 (TasksMax/pids sweep) is high value, especially if our harness uses `systemd-run` somewhere on the host side or if the guest itself is in a constrained cgroup.

### P4: The T1 signature is plausible but not guaranteed

- libc thread creation often involves:
  - mapping/reserving stack + guard region
  - applying protections (guard `PROT_NONE`, stack `PROT_READ|PROT_WRITE`)
  - `clone`/`clone3` to start the thread
- Different libcs/architectures/emulation layers can shift which syscall fails first.

Implication:

- A “T1-negative but pthread-positive” run should immediately push us toward T3 (broaden tracing and update the classifier to the observed chain), rather than treating T1 as the only success condition.

## Progress checklists (drive experiments, avoid drift)

### Baseline hygiene (before any run)

- [ ] Avoid the job-control confounder: no interactive controlling TTY wedge (`timeout --foreground` or PTY wrapper when applicable)
- [ ] Record the exact command in the run notes or repeat log
- [ ] Keep one independent variable per experiment (URL vs mem vs limits vs overcommit)
- [ ] Ensure artifacts are produced: `summary.txt`, `preflight.txt`, `stderr.txt` at minimum

### T0 checklist (completed vs killed)

- [ ] Add/confirm a `data:` URL control that should always complete
- [ ] For N≥5 runs, capture completion proxy (`stdout_bytes > 0`) vs `edge_exit`
- [ ] If `data:` completes but `https:` does not, prioritize H4/T4
- [ ] If `data:` does not complete reliably, treat H0 as primary and fix “completion is unambiguous” before deeper analysis

### T1 checklist (task/pids limits)

- [ ] Confirm `preflight.txt` includes: `/proc/self/limits`, `/proc/self/cgroup`, and pids controller state when available
- [ ] Run a sweep where only `TasksMax` / pids limit changes (small→large)
- [ ] For each sweep point, run N≥5 repeats and record the `pthread_create` rate
- [ ] If rate shifts sharply with the limit, reduce to a minimal repro and document the independent variable + effect size

### T2 checklist (memory/overcommit)

- [ ] Sweep only `--mem` first (e.g. 2048/4096/6144) with URL held constant
- [ ] Then flip only overcommit policy (one knob at a time)
- [ ] Compare rates of: `pthread_create`, completion, and any memory-related syscall failures in strace
- [ ] If overcommit/mem strongly changes rates, H2 strengthens; document and stop chasing unrelated hypotheses

### T3 checklist (extract real errno chain)

- [ ] On a pthread-positive run, find the earliest failing syscall/errno in the same pid/tid
- [ ] If failure is `clone/clone3 = -1 EAGAIN`, prioritize H1
- [ ] If failure is `mmap/mprotect = -1 ENOMEM`, prioritize H2 and update the classifier
- [ ] Update the classifier only to match observed reality (no speculative signatures)

### T4 checklist (URL class isolation)

- [ ] Hold mem/limits fixed; vary only the URL class
- [ ] Compare completion rate and stderr patterns by URL class
- [ ] If URL class dominates, treat as environment/I/O investigation and avoid “resource” overfitting

### T5 checklist (host/kernel/muvm/FEX contribution)

- [ ] Change only one component/version at a time
- [ ] Run N≥10 attempts (rates matter)
- [ ] If a version flip changes rates, extract a minimal repro and prepare for upstream reporting
