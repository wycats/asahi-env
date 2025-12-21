#!/usr/bin/env bash
set -euo pipefail

# Minimal, evidence-friendly scaffold for the "Edge via muvm" experiment.
#
# Goals:
# - Avoid downloads (you provide the RPM path).
# - Fail honestly when prerequisites are missing.
# - Keep state contained under a local workdir.
#
# Usage:
#   scripts/edge-muvm-experiment.sh [--mode preflight|edge] [--rpm /path/to/microsoft-edge-stable-*.rpm] [--workdir .local/edge-muvm]
#
# If you have already extracted the RPM (e.g. into .local/edge-muvm/extracted),
# the script will attempt a bounded headless run and capture a small summary.

workdir=".local/edge-muvm"
rpm=""
extracted_root=""
timeout_seconds="30"
url="https://example.com"
muvm_mem=""
mode="edge"
enable_strace="0"

abs_path() {
  python3 - "$1" <<'PY'
import os,sys
print(os.path.abspath(sys.argv[1]))
PY
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --mode)
      mode="$2"; shift 2 ;;
    --workdir)
      workdir="$2"; shift 2 ;;
    --rpm)
      rpm="$2"; shift 2 ;;
    --extracted-root)
      extracted_root="$2"; shift 2 ;;
    --timeout)
      timeout_seconds="$2"; shift 2 ;;
    --url)
      url="$2"; shift 2 ;;
    --mem)
      muvm_mem="$2"; shift 2 ;;
    --strace)
      enable_strace="1"; shift 1 ;;
    -h|--help)
      sed -n '1,80p' "$0"; exit 0 ;;
    *)
      echo "Unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

if [[ "$mode" != "preflight" && "$mode" != "edge" ]]; then
  echo "Unknown --mode: $mode (expected: preflight|edge)" >&2
  exit 2
fi

if [[ -n "$rpm" && ! -f "$rpm" ]]; then
  echo "RPM not found: $rpm" >&2
  exit 2
fi

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

need_cmd muvm
need_cmd timeout
need_cmd python3
need_cmd script

shell_escape() {
  # Escape a single string so it can be safely embedded in a shell command.
  # `printf %q` produces a representation that `bash` can round-trip.
  printf '%q' "$1"
}

strip_script_markers() {
  python3 - "$1" <<'PY'
import sys
path = sys.argv[1]
try:
  with open(path, 'rb') as f:
    lines = f.read().splitlines(True)
except FileNotFoundError:
  sys.exit(0)

if lines and lines[0].startswith(b"Script started on "):
  lines = lines[1:]
if lines and lines[-1].startswith(b"Script done on "):
  lines = lines[:-1]

with open(path, 'wb') as f:
  f.writelines(lines)
PY
}

if [[ "$mode" == "edge" ]]; then
  need_cmd rpm
fi

mkdir -p "$workdir"

# Use absolute paths so the VM's working directory doesn't matter.
workdir_abs="$(abs_path "$workdir")"
if [[ -n "$rpm" ]]; then
  rpm_abs="$(abs_path "$rpm")"
else
  rpm_abs=""
fi

if [[ -z "$extracted_root" ]]; then
  extracted_root="$workdir/extracted"
fi
extracted_root_abs="$(abs_path "$extracted_root")"

# Keep logs/artifacts together.
log="$workdir/run-$(date +%Y%m%d-%H%M%S).log"
{
  echo "== Edge via muvm experiment =="
  echo "date: $(date -Is)"
  echo "mode: $mode"
  if [[ -n "$rpm_abs" ]]; then
    echo "rpm:  $rpm_abs"
  else
    echo "rpm:  (none)"
  fi
  echo "work: $workdir_abs"
  echo

  echo "-- muvm info"
  muvm --help | sed -n '1,30p' || true
  echo

  if [[ "$mode" == "edge" ]]; then
    echo "-- rpm query (sanity)"
    if [[ -n "$rpm_abs" ]]; then
      rpm -qp "$rpm_abs" || true
    else
      echo "(skipped: no --rpm provided)"
    fi
    echo
  fi

  echo "-- NOTE"
  echo "This script intentionally does not download Edge or mutate system state beyond what muvm does."
  if [[ "$mode" == "edge" ]]; then
    echo "If you have already extracted the RPM into $extracted_root_abs, the script will try a bounded headless run."
    if [[ "$enable_strace" == "1" ]]; then
      echo "strace: enabled (captures thread + memory syscalls)"
    fi
  else
    echo "Preflight mode: validates muvm startup + shared filesystem writes."
  fi
} | tee "$log"

if [[ "$mode" == "preflight" ]]; then
  run_dir="$workdir_abs/preflight-$(date +%Y%m%d-%H%M%S)"
  mkdir -p "$run_dir"

  muvm_output_path="$run_dir/muvm.txt"
  summary_path="$run_dir/summary.txt"
  : >"$muvm_output_path"

  set +e
  start_ts="$(date +%s)"
  # Run muvm under a PTY to avoid known non-TTY and job-control edge cases.
  # `timeout --foreground` is critical when a controlling TTY is involved.
  script -q -e -c "timeout --foreground ${timeout_seconds}s muvm --emu=fex -e RUN_DIR=$run_dir bash -lc 'set -euo pipefail; echo \"hello\" >\"\$RUN_DIR/vm-ok.txt\"; echo \"wrote:\$RUN_DIR/vm-ok.txt\"'" \
    "$muvm_output_path"
  rc=$?
  strip_script_markers "$muvm_output_path" || true
  end_ts="$(date +%s)"
  set -e

  {
    echo "exit_code: $rc"
    echo "elapsed_seconds: $((end_ts - start_ts))"
    echo "run_dir: $run_dir"
    echo "vm_ok_exists: $(test -f "$run_dir/vm-ok.txt" && echo yes || echo no)"
    echo
    echo "muvm_output_preview:"
    sed -n '1,80p' "$muvm_output_path" || true
  } | tee "$summary_path" | tee -a "$log"

  echo "Wrote log: $log"
  exit 0
fi

if [[ -d "$extracted_root_abs" ]]; then
  edge_bin="$extracted_root_abs/opt/microsoft/msedge/microsoft-edge"
  if [[ -x "$edge_bin" ]]; then
    run_dir="$workdir_abs/headless-$(date +%Y%m%d-%H%M%S)"
    mkdir -p "$run_dir"

    stdout_path="$run_dir/stdout.txt"
    stderr_path="$run_dir/stderr.txt"
    stderr_filtered_path="$run_dir/stderr.filtered.txt"
    preflight_path="$run_dir/preflight.txt"
    ps_path="$run_dir/ps.txt"
    threads_path="$run_dir/threads.txt"
    summary_path="$run_dir/summary.txt"
    vm_script_path="$run_dir/run-in-vm.sh"
    muvm_output_path="$run_dir/muvm.txt"
    host_watch_path="$run_dir/host-watch.txt"
    host_strace_status_path="$run_dir/host.strace.status.txt"

    echo >>"$log"
    echo "-- headless run" | tee -a "$log"
    echo "dir: $run_dir" | tee -a "$log"
    echo "url: $url" | tee -a "$log"
    echo "timeout: ${timeout_seconds}s" | tee -a "$log"
    echo "edge: $edge_bin" | tee -a "$log"

    muvm_args=(--emu=fex)
    if [[ -n "$muvm_mem" ]]; then
      muvm_args+=("--mem=$muvm_mem")
    fi

    # Avoid inheriting host DBus session env into a VM that doesn't have that bus.
    muvm_args+=(
      -e DBUS_SESSION_BUS_ADDRESS=
      -e DBUS_SYSTEM_BUS_ADDRESS=
      -e XDG_RUNTIME_DIR=
    )

    # Pass run parameters into the VM via environment variables.
    # Note: We still need to carefully escape args when embedding them into `script -c`.
    edge_timeout_seconds_guest="$timeout_seconds"
    if [[ "$edge_timeout_seconds_guest" -gt 15 ]]; then
      edge_timeout_seconds_guest=$((edge_timeout_seconds_guest - 10))
    fi
    if [[ "$edge_timeout_seconds_guest" -lt 5 ]]; then
      edge_timeout_seconds_guest=5
    fi

    muvm_args+=(
      -e "EDGE_BIN=$edge_bin"
      -e "RUN_DIR=$run_dir"
      -e "URL=$url"
      -e "EDGE_TIMEOUT_SECONDS=$edge_timeout_seconds_guest"
      -e "EDGE_STRACE=$enable_strace"
    )

    cat >"$vm_script_path" <<'VM_SCRIPT'
#!/usr/bin/env bash
set -euo pipefail

{
  echo "date: $(date -Is)"
  echo "cwd: $(pwd -P)"
  echo "EDGE_BIN=$EDGE_BIN"
  echo "RUN_DIR=$RUN_DIR"
  echo "URL=$URL"
  echo
  echo "ulimit -a:"
  ulimit -a || true
  echo
  echo "/proc/self/limits:"
  cat /proc/self/limits || true
  echo
  echo "kernel pid/thread limits:"
  echo "  pid_max: $(cat /proc/sys/kernel/pid_max 2>/dev/null || echo unknown)"
  echo "  threads-max: $(cat /proc/sys/kernel/threads-max 2>/dev/null || echo unknown)"
  echo "  vm.max_map_count: $(cat /proc/sys/vm/max_map_count 2>/dev/null || echo unknown)"
  echo
  echo "vm overcommit settings:"
  echo "  vm.overcommit_memory: $(cat /proc/sys/vm/overcommit_memory 2>/dev/null || echo unknown)"
  echo "  vm.overcommit_ratio: $(cat /proc/sys/vm/overcommit_ratio 2>/dev/null || echo unknown)"
  echo "  vm.overcommit_kbytes: $(cat /proc/sys/vm/overcommit_kbytes 2>/dev/null || echo unknown)"
  echo
  echo "cgroup pids.max:"
  if [[ -f /sys/fs/cgroup/pids.max ]]; then
    echo "  /sys/fs/cgroup/pids.max: $(cat /sys/fs/cgroup/pids.max 2>/dev/null || echo unknown)"
  else
    echo "  (no /sys/fs/cgroup/pids.max)"
  fi
  echo
  echo "/proc/self/cgroup:"
  cat /proc/self/cgroup 2>/dev/null || true
  echo
  echo "cgroup mounts (/proc/self/mountinfo grep cgroup):"
  grep -E ' - cgroup2? ' /proc/self/mountinfo 2>/dev/null || true
  echo
  echo "discover pids.max files (first 25):"
  if command -v find >/dev/null 2>&1; then
    n=0
    while IFS= read -r f; do
      n=$((n + 1))
      echo "  [$n] $f: $(cat "$f" 2>/dev/null || echo unknown)"
    done < <(find /sys/fs/cgroup -name pids.max -type f 2>/dev/null | head -n 25)
    if [[ "$n" -eq 0 ]]; then
      echo "  (none found under /sys/fs/cgroup)"
    fi
  else
    echo "  (find not available)"
  fi
  echo
  echo "meminfo (first 20 lines):"
  head -n 20 /proc/meminfo 2>/dev/null || true
  echo
  echo "meminfo commit accounting:"
  grep -E '^(CommitLimit|Committed_AS):' /proc/meminfo 2>/dev/null || true
  echo
  echo "/dev/shm:"
  df -h /dev/shm 2>/dev/null || true
  echo
  echo "ls_edge_bin:"
  ls -l "$EDGE_BIN" || true
  echo
  echo "ls_run_dir:"
  ls -ld "$RUN_DIR" || true
} >"$RUN_DIR/preflight.txt" 2>&1

mkdir -p "$RUN_DIR/profile"

# Ensure artifacts exist even if Edge fails early.
: >"$RUN_DIR/stdout.txt"
: >"$RUN_DIR/stderr.txt"

# Lower per-thread stack to reduce the chance that thread creation fails under tight memory.
ulimit -s 1024 || true

maybe_strace_prefix=()
if [[ "${EDGE_STRACE:-0}" == "1" ]]; then
  if command -v strace >/dev/null 2>&1; then
    {
      echo "strace: yes"
      strace -V 2>&1 | head -n 1 || true
    } >"$RUN_DIR/strace.enabled.txt" 2>&1
    # Capture the likely syscall paths behind pthread_create failures.
    # -ff splits per-thread/process; output files will be $RUN_DIR/strace.<pid>
    # Keep this intentionally narrow to avoid massive traces.
    strace_trace_set="clone,clone3,mmap,mprotect,munmap,mremap,brk,futex,prlimit64,setrlimit"
    maybe_strace_prefix=(strace -ff -tt -s 0 -o "$RUN_DIR/strace" -e trace="$strace_trace_set")
  else
    echo "strace: requested but not available" >"$RUN_DIR/strace.enabled.txt"
  fi
fi

"${maybe_strace_prefix[@]}" "$EDGE_BIN" \
  --headless \
  --disable-gpu \
  --no-first-run \
  --no-default-browser-check \
  --password-store=basic \
  --use-mock-keychain \
  --disable-breakpad \
  --disable-crash-reporter \
  --no-crash-upload \
  --disable-features=Crashpad \
  --user-data-dir="$RUN_DIR/profile" \
  --dump-dom "$URL" \
  >"$RUN_DIR/stdout.txt" 2>"$RUN_DIR/stderr.txt" &

pid=$!
echo "edge_pid=$pid" >"$RUN_DIR/pid.txt"

# Give it a moment to start, then snapshot process state.
sleep 3
{
  echo "### ps -o pid,ppid,etime,cmd (edge pid)"
  ps -o pid,ppid,etime,cmd -p "$pid" || true
  echo
  echo "### ps -ef (edge-related, first 50)"
  ps -ef | grep -E "(microsoft-edge|msedge|chrome|crashpad|FEXInterpreter)" | grep -v grep | head -n 50 || true
} >"$RUN_DIR/ps.txt"

{
  echo "### edge /proc/$pid/status"
  cat "/proc/$pid/status" 2>/dev/null || true
  echo
  echo "### system process counts (3s snapshot)"
  echo -n "processes_total="
  ps -e 2>/dev/null | wc -l 2>/dev/null || true
  echo -n "zombies_total="
  ps -e -o stat= 2>/dev/null | grep -c '^Z' 2>/dev/null || true
  echo -n "ns_last_pid="
  cat /proc/sys/kernel/ns_last_pid 2>/dev/null || echo unknown
  echo
  echo "### edge /proc/$pid/limits"
  cat "/proc/$pid/limits" 2>/dev/null || true
  echo
  echo "### edge map count (wc -l /proc/$pid/maps)"
  wc -l "/proc/$pid/maps" 2>/dev/null || true
} >"$RUN_DIR/edge-proc-3s.txt"

{
  echo "### thread_count_total"
  ps -eT | wc -l || true
  echo "### thread_count_edge"
  ps -T -p "$pid" 2>/dev/null | wc -l || true
} >"$RUN_DIR/threads.txt"

edge_exit_path="$RUN_DIR/edge-exit.txt"

# Wait for Edge to finish, but keep the run bounded so --dump-dom has time to work.
# (We also have a host-side timeout around muvm, but this ensures we collect an exit code.)
limit="${EDGE_TIMEOUT_SECONDS:-30}"
if [[ "$limit" -lt 5 ]]; then
  limit=5
fi
deadline=$(( $(date +%s) + limit - 2 ))

while kill -0 "$pid" 2>/dev/null; do
  now="$(date +%s)"
  if [[ "$now" -ge "$deadline" ]]; then
    echo "edge_killed=timeout" >"$edge_exit_path"
    kill "$pid" 2>/dev/null || true
    sleep 1
    kill -KILL "$pid" 2>/dev/null || true
    break
  fi
  sleep 0.2
done

set +e
wait "$pid"
rc=$?
set -e
echo "edge_exit_code=$rc" >>"$edge_exit_path"

# If we captured strace output, extract evidence of syscall-level failures.
if [[ -n "${EDGE_STRACE:-}" && "${EDGE_STRACE}" == "1" ]]; then
  if ls "$RUN_DIR"/strace.* >/dev/null 2>&1; then
    {
      echo "### clone/clone3 failures (first 200 lines)"
      grep -hE ' clone3?\(' "$RUN_DIR"/strace.* 2>/dev/null | grep -E '= -1 ' | head -n 200 || true
      echo
      echo "### clone/clone3 failure count"
      grep -hE ' clone3?\(' "$RUN_DIR"/strace.* 2>/dev/null | grep -c -E '= -1 ' || true
    } >"$RUN_DIR/strace.clone.eagain.txt" 2>&1

    {
      echo "### selected syscalls failing (first 200 lines)"
      grep -hE ' (clone3?|mmap|mprotect|munmap|mremap|brk|futex|prlimit64|setrlimit)\(' "$RUN_DIR"/strace.* 2>/dev/null \
        | grep -E '= -1 ' \
        | head -n 200 || true
      echo
      echo "### selected syscalls failing (last 200 lines)"
      grep -hE ' (clone3?|mmap|mprotect|munmap|mremap|brk|futex|prlimit64|setrlimit)\(' "$RUN_DIR"/strace.* 2>/dev/null \
        | grep -E '= -1 ' \
        | tail -n 200 || true
      echo
      echo "### failure errno counts (selected syscalls)"
      grep -hE ' (clone3?|mmap|mprotect|munmap|mremap|brk|futex|prlimit64|setrlimit)\(' "$RUN_DIR"/strace.* 2>/dev/null \
        | awk 'match($0, /= -1 ([A-Z0-9_]+)/, m) {print m[1]}' \
        | sort \
        | uniq -c \
        | sort -nr \
        | head -n 30 || true
    } >"$RUN_DIR/strace.failures.txt" 2>&1
  fi
fi
VM_SCRIPT
    chmod +x "$vm_script_path"

  # Capture muvm output to a file (keeps terminal noise low, preserves signal).
  : >"$muvm_output_path"

    # Run through bash inside the VM so we can:
    # - write logs to host-visible paths (the repo checkout is shared)
    # - sample process/thread state without spamming the host terminal
    # - bound execution to preserve sanity
    set +e
    start_ts="$(date +%s)"
    # Run muvm under a PTY to avoid known non-TTY and job-control edge cases.
    # `timeout --foreground` is critical when a controlling TTY is involved.
    muvm_args_escaped="$(printf '%q ' "${muvm_args[@]}")"
    vm_script_path_escaped="$(shell_escape "$vm_script_path")"
    : >"$host_watch_path"
    : >"$host_strace_status_path"

    # Host-side watcher: capture cgroup pids pressure while muvm runs.
    # This helps distinguish guest limits from host cgroup limits (threads count as pids in cgroup v2).
    (
      set +e
      start="$(date +%s)"
      while :; do
        now="$(date +%s)"
        if [[ $((now - start)) -ge ${timeout_seconds} ]]; then
          break
        fi

        muvm_pid="$(pgrep -f "muvm .*bash ${vm_script_path}" | head -n 1)"
        {
          echo "ts=$(date -Is)"
          if [[ -n "$muvm_pid" && -r "/proc/$muvm_pid/cgroup" ]]; then
            echo "muvm_pid=$muvm_pid"
            cg_rel="$(awk -F: '$1==0{print $3}' "/proc/$muvm_pid/cgroup" 2>/dev/null)"
            echo "muvm_cgroup=$cg_rel"
            cg_base="/sys/fs/cgroup${cg_rel}"
            if [[ -r "$cg_base/pids.current" ]]; then
              echo "pids.current=$(cat "$cg_base/pids.current" 2>/dev/null)"
            else
              echo "pids.current=unavailable"
            fi
            if [[ -r "$cg_base/pids.max" ]]; then
              echo "pids.max=$(cat "$cg_base/pids.max" 2>/dev/null)"
            else
              echo "pids.max=unavailable"
            fi
            if [[ -r "$cg_base/cgroup.events" ]]; then
              echo "cgroup.events:"
              sed -n '1,20p' "$cg_base/cgroup.events" 2>/dev/null || true
            fi
            echo -n "host_threads_total="
            ps -eT 2>/dev/null | wc -l 2>/dev/null || true
            echo -n "host_fex_threads="
            ps -eT -o comm= 2>/dev/null | grep -c '^FEXInterpreter$' 2>/dev/null || true
          else
            echo "muvm_pid=(not found yet)"
          fi
          echo
        } >>"$host_watch_path" 2>&1

        sleep 0.5
      done
    ) &
    host_watch_pid=$!

    # If --strace was requested, but the guest doesn't have strace, try host-side attachment.
    # This only works if guest PIDs are visible on the host (same kernel/namespace view).
    (
      set +e
      if [[ "$enable_strace" != "1" ]]; then
        exit 0
      fi
      if ! command -v strace >/dev/null 2>&1; then
        echo "host_strace: unavailable (strace not installed on host)" >>"$host_strace_status_path"
        exit 0
      fi
      pid_file="$run_dir/pid.txt"
      deadline=$(( $(date +%s) + 12 ))
      while [[ ! -f "$pid_file" && $(date +%s) -lt $deadline ]]; do
        sleep 0.1
      done
      if [[ ! -f "$pid_file" ]]; then
        echo "host_strace: no pid.txt" >>"$host_strace_status_path"
        exit 0
      fi
      edge_pid="$(awk -F= '/^edge_pid=/{print $2}' "$pid_file" 2>/dev/null | head -n1)"
      if [[ -z "$edge_pid" ]]; then
        echo "host_strace: could not parse edge_pid" >>"$host_strace_status_path"
        exit 0
      fi
      if [[ ! -d "/proc/$edge_pid" ]]; then
        echo "host_strace: pid $edge_pid not visible on host" >>"$host_strace_status_path"
        exit 0
      fi
      echo "host_strace: attaching to pid=$edge_pid" >>"$host_strace_status_path"
      # Run briefly; enough to catch a burst of clone failures if that's the mechanism.
      strace_trace_set="clone,clone3,mmap,mprotect,munmap,mremap,brk,futex,prlimit64,setrlimit"
      timeout --foreground 6s strace -ff -tt -s 0 -o "$run_dir/host.strace" -e trace="$strace_trace_set" -p "$edge_pid" 2>&1
      rc=$?
      echo "host_strace_rc=$rc" >>"$host_strace_status_path"
      if ls "$run_dir"/host.strace.* >/dev/null 2>&1; then
        {
          echo "### clone/clone3 EAGAIN (first 200 lines)"
          grep -hE ' clone3?\(' "$run_dir"/host.strace.* 2>/dev/null | grep -E '= -1 ' | head -n 200 || true
          echo
          echo "### clone/clone3 failure count"
          grep -hE ' clone3?\(' "$run_dir"/host.strace.* 2>/dev/null | grep -c -E '= -1 ' || true
        } >"$run_dir/host.strace.clone.eagain.txt" 2>&1

        {
          echo "### selected syscalls failing (first 200 lines)"
          grep -hE ' (clone3?|mmap|mprotect|munmap|mremap|brk|futex|prlimit64|setrlimit)\(' "$run_dir"/host.strace.* 2>/dev/null \
            | grep -E '= -1 ' \
            | head -n 200 || true
          echo
          echo "### selected syscalls failing (last 200 lines)"
          grep -hE ' (clone3?|mmap|mprotect|munmap|mremap|brk|futex|prlimit64|setrlimit)\(' "$run_dir"/host.strace.* 2>/dev/null \
            | grep -E '= -1 ' \
            | tail -n 200 || true
          echo
          echo "### failure errno counts (selected syscalls)"
          grep -hE ' (clone3?|mmap|mprotect|munmap|mremap|brk|futex|prlimit64|setrlimit)\(' "$run_dir"/host.strace.* 2>/dev/null \
            | awk 'match($0, /= -1 ([A-Z0-9_]+)/, m) {print m[1]}' \
            | sort \
            | uniq -c \
            | sort -nr \
            | head -n 30 || true
        } >"$run_dir/host.strace.failures.txt" 2>&1
      fi
    ) &
    host_strace_pid=$!

    script -q -e -c "timeout --foreground ${timeout_seconds}s muvm ${muvm_args_escaped} bash ${vm_script_path_escaped}" \
      "$muvm_output_path"
    rc=$?

    # Stop watcher (best effort).
    kill "$host_watch_pid" 2>/dev/null || true
    kill "$host_strace_pid" 2>/dev/null || true
    strip_script_markers "$muvm_output_path" || true
    end_ts="$(date +%s)"
    set -e

    if [[ ! -f "$stdout_path" || ! -f "$stderr_path" ]]; then
      {
        echo "exit_code: $rc"
        echo "elapsed_seconds: $((end_ts - start_ts))"
        echo "note: expected artifacts missing"
        echo
        echo "host_paths:"
        echo "  run_dir: $run_dir"
        echo "  vm_script: $vm_script_path"
        echo "  muvm_output: $muvm_output_path"
        echo
        echo "host_ls_run_dir:"
        ls -la "$run_dir" || true
        echo
        echo "muvm_output_preview:"
        sed -n '1,120p' "$muvm_output_path" || true
      } | tee "$summary_path" | tee -a "$log"
      echo "Wrote log: $log"
      exit 1
    fi

    # Filter out the very noisy crashpad/ptrace spam for quick review.
    grep -Ev 'crashpad|ptrace:' "$stderr_path" >"$stderr_filtered_path" || true

    stdout_bytes="$(wc -c <"$stdout_path" | tr -d ' ')"
    stderr_lines="$(wc -l <"$stderr_path" | tr -d ' ')"
    stderr_filtered_lines="$(wc -l <"$stderr_filtered_path" | tr -d ' ')"
    ptrace_lines="$(grep -c 'ptrace:' "$stderr_path" 2>/dev/null || true)"
    pthread_lines="$(grep -c 'pthread_create' "$stderr_path" 2>/dev/null || true)"
    dbus_lines="$(grep -c 'Failed to connect to the bus' "$stderr_path" 2>/dev/null || true)"
    ssl_lines="$(grep -c 'ssl_client_socket_impl.cc:930' "$stderr_path" 2>/dev/null || true)"
    handshake_lines="$(grep -c 'handshake failed' "$stderr_path" 2>/dev/null || true)"
    crashpad_handler_lines="$(grep -c 'crashpad_handler' "$ps_path" 2>/dev/null || true)"

    pthread_stack_report_path="$run_dir/pthread.stack-mprotect-enomem.txt"
    pthread_stack_events=""
    pthread_stack_pids=""

    if [[ "$enable_strace" == "1" ]]; then
      # Classify the *actionable* failure mode:
      #   mmap(... MAP_STACK ...) = <addr>
      #   mprotect(<addr>, <size>, PROT_READ|PROT_WRITE) = -1 ENOMEM
      # in the same PID that logged pthread_create(EAGAIN).
      python3 - "$stderr_path" "$run_dir" >"$pthread_stack_report_path" 2>&1 <<'PY'
import os
import re
import sys

stderr_path = sys.argv[1]
run_dir = sys.argv[2]

id_re = re.compile(r"\[(\d+):(\d+)\b")

def read_lines(path: str):
  try:
    with open(path, "r", encoding="utf-8", errors="replace") as f:
      return f.read().splitlines()
  except FileNotFoundError:
    return []

stderr_lines = read_lines(stderr_path)

pthread_ids: list[tuple[str, str]] = []
seen = set()
for line in stderr_lines:
  if "pthread_create" not in line:
    continue
  m = id_re.search(line)
  if not m:
    continue
  pid, tid = m.group(1), m.group(2)
  key = (pid, tid)
  if key not in seen:
    seen.add(key)
    pthread_ids.append(key)

pthread_pids: list[str] = []
seen_pids = set()
for pid, _tid in pthread_ids:
  if pid not in seen_pids:
    seen_pids.add(pid)
    pthread_pids.append(pid)

def pick_strace_path(pid: str, tid: str):
  # Chromium logs as [pid:tid]. Under strace -ff, the output file is commonly keyed
  # by the thread ID, not the process ID. Prefer tid.
  candidates: list[tuple[str, str]] = [(tid, "tid"), (pid, "pid")]
  for ident, kind in candidates:
    for prefix in ("strace.", "host.strace."):
      p = os.path.join(run_dir, f"{prefix}{ident}")
      if os.path.exists(p):
        return p, ident, kind
  return None, None, None

mmap_re = re.compile(r"\bmmap\([^\n]*MAP_STACK[^\n]*\)\s*=\s*(0x[0-9a-fA-F]+)")

def is_fatal_stack_mprotect(line: str, addr: str) -> bool:
  if "mprotect(" not in line:
    return False
  if addr not in line:
    return False
  if "PROT_READ|PROT_WRITE" not in line:
    return False
  if "= -1 ENOMEM" not in line:
    return False
  return True

def extract_context(lines: list[str], idx: int, before: int = 3, after: int = 3) -> list[str]:
  lo = max(0, idx - before)
  hi = min(len(lines), idx + after + 1)
  return lines[lo:hi]

events_total = 0
print("pthread_ids_from_stderr:", " ".join(f"{pid}:{tid}" for pid, tid in pthread_ids) if pthread_ids else "(none)")
print("pthread_pids_from_stderr:", " ".join(pthread_pids) if pthread_pids else "(none)")

for pid, tid in pthread_ids:
  strace_path, ident, ident_kind = pick_strace_path(pid, tid)
  if not strace_path:
    print(f"\n== pid {pid} tid {tid} ==")
    print("strace: (missing)")
    continue

  lines = read_lines(strace_path)
  print(f"\n== pid {pid} tid {tid} ==")
  print("strace:", f"{os.path.basename(strace_path)} (matched {ident_kind}={ident})")

  # Find MAP_STACK mmaps, then look forward for the matching ENOMEM mprotect.
  pid_events = 0
  for i, line in enumerate(lines):
    mm = mmap_re.search(line)
    if not mm:
      continue
    addr = mm.group(1)
    # Heuristic: look ahead a bounded window for a failing mprotect on the same addr.
    for j in range(i + 1, min(i + 250, len(lines))):
      if is_fatal_stack_mprotect(lines[j], addr):
        pid_events += 1
        events_total += 1
        print(f"\n-- stack mprotect ENOMEM event #{pid_events} --")
        for ctx in extract_context(lines, j, before=5, after=3):
          print(ctx)
        break

  if pid_events == 0:
    print("stack_mprotect_enomem_events: 0")
  else:
    print(f"stack_mprotect_enomem_events: {pid_events}")

print(f"\nstack_mprotect_enomem_events_total: {events_total}")
PY

      pthread_stack_events="$(awk -F': ' '/^stack_mprotect_enomem_events_total:/{print $2; exit}' "$pthread_stack_report_path" 2>/dev/null || true)"
      pthread_stack_ids="$(awk -F': ' '/^pthread_ids_from_stderr:/{print $2; exit}' "$pthread_stack_report_path" 2>/dev/null || true)"
      pthread_stack_pids="$(awk -F': ' '/^pthread_pids_from_stderr:/{print $2; exit}' "$pthread_stack_report_path" 2>/dev/null || true)"
    fi

    {
      echo "exit_code: $rc"
      echo "elapsed_seconds: $((end_ts - start_ts))"
      echo "stdout_bytes: $stdout_bytes"
      echo "stderr_lines: $stderr_lines"
      echo "stderr_ptrace_lines: $ptrace_lines"
      echo "stderr_pthread_create_lines: $pthread_lines"
      if [[ -n "$pthread_stack_events" ]]; then
        echo "pthread_stack_mprotect_enomem_events: $pthread_stack_events"
      fi
      if [[ -n "$pthread_stack_pids" ]]; then
        echo "pthread_pids_from_stderr: $pthread_stack_pids"
      fi
      if [[ -n "${pthread_stack_ids:-}" ]]; then
        echo "pthread_ids_from_stderr: $pthread_stack_ids"
      fi
      echo "stderr_dbus_lines: $dbus_lines"
      echo "stderr_ssl_client_socket_lines: $ssl_lines"
      echo "stderr_handshake_failed_lines: $handshake_lines"
      echo "ps_crashpad_handler_lines: $crashpad_handler_lines"
      echo
      echo "artifacts:"
      echo "  preflight: $preflight_path"
      echo "  ps: $ps_path"
      echo "  threads: $threads_path"
      echo "  host_watch: $host_watch_path"
      echo "  stdout: $stdout_path"
      echo "  stderr: $stderr_path"
      echo "  stderr_filtered: $stderr_filtered_path"
      if [[ -f "$pthread_stack_report_path" ]]; then
        echo "  pthread_stack_report: $pthread_stack_report_path"
      fi
    } | tee "$summary_path" | tee -a "$log"

    if [[ "$stdout_bytes" -gt 0 ]]; then
      echo "-- stdout preview (first 40 lines)" | tee -a "$log"
      sed -n '1,40p' "$stdout_path" | tee -a "$log"
    fi

    if [[ "$stderr_filtered_lines" -gt 0 ]]; then
      echo "-- stderr.filtered preview (first 30 lines)" | tee -a "$log"
      sed -n '1,30p' "$stderr_filtered_path" | tee -a "$log"
    else
      echo "-- stderr.filtered is empty" | tee -a "$log"
    fi
  else
    echo >>"$log"
    echo "-- extracted root found, but Edge binary missing or not executable" | tee -a "$log"
    echo "expected: $edge_bin" | tee -a "$log"
  fi
else
  echo >>"$log"
  echo "-- no extracted root present; skipping headless run" | tee -a "$log"
  echo "expected: $extracted_root_abs" | tee -a "$log"
fi

echo "Wrote log: $log"