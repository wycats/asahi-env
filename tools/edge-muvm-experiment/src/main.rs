use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::fs;
use std::io::{self, Write};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(about = "Evidence-friendly Edge via muvm experiment runner", long_about = None)]
struct Cli {
    /// Experiment mode.
    #[arg(long, value_enum, default_value_t = Mode::Edge)]
    mode: Mode,

    /// Work directory for logs/artifacts.
    #[arg(long, default_value = ".local/edge-muvm")]
    workdir: PathBuf,

    /// Optional path to the Edge RPM (only used for metadata logging today).
    #[arg(long)]
    rpm: Option<PathBuf>,

    /// Path to an already extracted RPM root.
    ///
    /// If omitted, defaults to `<workdir>/extracted`.
    #[arg(long)]
    extracted_root: Option<PathBuf>,

    /// Timeout in seconds for the muvm invocation.
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// Watchdog in seconds for the Edge process inside the guest.
    ///
    /// If Edge has not exited within this window, the guest-runner will capture a stuck
    /// snapshot and then SIGKILL the process to keep runs bounded.
    #[arg(long, default_value_t = 45)]
    edge_watchdog_seconds: u64,

    /// (muvm-true-matrix) Number of runs per case.
    #[arg(long, default_value_t = 3)]
    matrix_runs: u32,

    /// URL to load for headless mode.
    #[arg(long, default_value = "https://example.com")]
    url: String,

    /// Select Chromium headless implementation.
    ///
    /// `new` uses the default modern headless mode (`--headless`).
    /// `old` forces legacy headless (`--headless=old`).
    #[arg(long, value_enum, default_value_t = HeadlessImpl::New)]
    headless_impl: HeadlessImpl,

    /// Extra args to pass to `microsoft-edge` (repeatable).
    ///
    /// Example: `--edge-arg=--no-sandbox`.
    #[arg(long, allow_hyphen_values = true)]
    edge_arg: Vec<String>,

    /// Extra environment variables to set for the Edge process (repeatable).
    ///
    /// Example: `--edge-env=CHROME_HEADLESS=1`.
    #[arg(long, value_name = "KEY=VALUE")]
    edge_env: Vec<String>,

    /// Preserve DBus/XDG environment variables when invoking `muvm`.
    ///
    /// By default we clear `DBUS_SESSION_BUS_ADDRESS` and `XDG_RUNTIME_DIR` to avoid
    /// inheriting host session env into a VM that may not have those sockets.
    ///
    /// This flag disables that clearing so we can test whether Chromium blocks
    /// on DBus/runtime-dir discovery.
    #[arg(long, default_value_t = false)]
    preserve_dbus_xdg_env: bool,

    /// Best-effort guest sysctl writes to apply before spawning Edge.
    ///
    /// Example: `--guest-sysctl=vm.overcommit_memory=1`.
    ///
    /// Values are written inside the guest to `/proc/sys/...` and failures are
    /// logged (runs continue even if a write fails).
    #[arg(long, value_name = "KEY=VALUE")]
    guest_sysctl: Vec<String>,

    /// Where to place the Edge profile directory.
    ///
    /// `shared` uses `<run_dir>/profile` (virtio-fs/shared).
    /// `guest-tmp` uses a per-run directory under `/tmp` inside the guest.
    #[arg(long, value_enum, default_value_t = ProfileLocation::Shared)]
    profile_location: ProfileLocation,

    /// Memory for muvm, e.g. 4096.
    #[arg(long)]
    mem: Option<u64>,

    /// Run the command as root inside the VM (`muvm --privileged`).
    ///
    /// This is required for experiments that attempt to write guest sysctls
    /// (e.g. `vm.overcommit_memory=1`).
    #[arg(long, default_value_t = false)]
    muvm_privileged: bool,

    /// Enable syscall tracing inside the guest (requires `strace` in the guest rootfs).
    ///
    /// Produces per-thread/process traces under the run dir as `strace.<id>` files.
    #[arg(long, default_value_t = false)]
    strace: bool,

    /// Select which syscalls `strace` should capture (only relevant when `--strace` is enabled).
    #[arg(long, value_enum, default_value_t = StraceMode::Minimal)]
    strace_mode: StraceMode,

    /// (edge-repeat) Maximum attempts before stopping.
    #[arg(long, default_value_t = 6)]
    repeat_max_attempts: u32,

    /// (edge-repeat) Stop condition.
    #[arg(long, value_enum, default_value_t = RepeatStopOn::PthreadCreate)]
    repeat_stop_on: RepeatStopOn,

    /// Wrap `muvm` in `systemd-run --user --pty --wait -p TasksMax=<N> -- ...`.
    ///
    /// This is useful for testing whether a systemd cgroup task/thread limit is causing
    /// Chromium/Edge failures like `pthread_create: Resource temporarily unavailable`.
    #[arg(long)]
    systemd_tasks_max: Option<u64>,

    /// (guest-runner) Absolute path to Edge binary inside the VM.
    #[arg(long)]
    edge_bin: Option<PathBuf>,

    /// (guest-runner) Absolute run directory shared with host.
    #[arg(long)]
    run_dir: Option<PathBuf>,

    /// (guest-runner) Headless implementation selector.
    #[arg(long, value_enum, default_value_t = HeadlessImpl::New)]
    guest_headless_impl: HeadlessImpl,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum RepeatStopOn {
    /// Stop once stderr contains any `pthread_create` lines.
    PthreadCreate,
    /// Stop once the T1 classifier finds a stack `mprotect(...)=ENOMEM` event.
    StackMprotectEnomem,
    /// Stop once stdout is non-empty (i.e., `--dump-dom` produced output).
    StdoutNonEmpty,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum HeadlessImpl {
    New,
    Old,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ProfileLocation {
    Shared,
    GuestTmp,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum StraceMode {
    /// Keep traces small and focused on thread creation / memory mapping.
    Minimal,
    /// Hang-focused tracing (process+signal+ipc+network+fds+memory) with syscall timings.
    Hang,
}

impl ProfileLocation {
    fn as_arg(&self) -> &'static str {
        match self {
            ProfileLocation::Shared => "shared",
            ProfileLocation::GuestTmp => "guest-tmp",
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Mode {
    Preflight,
    MuvmTrue,
    MuvmTrueMatrix,
    Edge,
    EdgeRepeat,
    /// Analyze an existing run dir on the host (re-runs classifiers; does not invoke muvm).
    AnalyzeRunDir,
    GuestRunner,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Guest-runner mode executes *inside* the VM and must not attempt to invoke muvm.
    if let Mode::GuestRunner = cli.mode {
        let edge_bin = cli
            .edge_bin
            .as_deref()
            .context("--edge-bin is required in guest-runner mode")?;
        let run_dir = cli
            .run_dir
            .as_deref()
            .context("--run-dir is required in guest-runner mode")?;
        return guest_runner(
            edge_bin,
            run_dir,
            &cli.url,
            cli.guest_headless_impl,
            &cli.edge_arg,
            &cli.edge_env,
            cli.profile_location,
            cli.preserve_dbus_xdg_env,
            &cli.guest_sysctl,
            cli.strace,
            cli.strace_mode,
            Duration::from_secs(cli.edge_watchdog_seconds),
        );
    }

    // Resolve host-side helpers up-front so PTY execution isn't dependent on PATH quirks.
    let muvm_path = resolve_in_path("muvm").context("locate muvm in PATH")?;
    let systemd_run_path = if cli.systemd_tasks_max.is_some() {
        Some(resolve_in_path("systemd-run").context("locate systemd-run in PATH")?)
    } else {
        None
    };

    fs::create_dir_all(&cli.workdir).context("create workdir")?;
    let workdir_abs = fs::canonicalize(&cli.workdir).context("canonicalize workdir")?;

    let extracted_root = cli
        .extracted_root
        .clone()
        .unwrap_or_else(|| cli.workdir.join("extracted"));
    let extracted_root_abs = if extracted_root.exists() {
        fs::canonicalize(&extracted_root).context("canonicalize extracted root")?
    } else {
        extracted_root
    };

    let log_path = workdir_abs.join(format!("run-{}-{:?}.log", chrono_stamp(), cli.mode));
    {
        let mut f = fs::File::create(&log_path).context("create run log")?;
        writeln!(f, "== Edge via muvm experiment ==")?;
        writeln!(f, "date: {}", iso_now())?;
        writeln!(f, "mode: {:?}", cli.mode)?;
        writeln!(f, "work: {}", workdir_abs.display())?;
        writeln!(f, "extracted_root: {}", extracted_root_abs.display())?;
        writeln!(
            f,
            "systemd_tasks_max: {}",
            cli.systemd_tasks_max
                .map(|v| v.to_string())
                .unwrap_or_else(|| "(none)".to_string())
        )?;
        writeln!(
            f,
            "systemd_run: {}",
            systemd_run_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(not used)".to_string())
        )?;
        if let Some(rpm) = &cli.rpm {
            writeln!(f, "rpm: {}", rpm.display())?;
        } else {
            writeln!(f, "rpm: (none)")?;
        }
        writeln!(f)?;
        writeln!(f, "-- NOTE")?;
        writeln!(
            f,
            "This tool does not download Edge or mutate system state beyond what muvm does."
        )?;
    }

    match cli.mode {
        Mode::Preflight => run_preflight(
            &muvm_path,
            systemd_run_path.as_deref(),
            cli.systemd_tasks_max,
            &workdir_abs,
            cli.timeout,
        )?,
        Mode::MuvmTrue => run_muvm_true(
            &muvm_path,
            systemd_run_path.as_deref(),
            cli.systemd_tasks_max,
            &workdir_abs,
            cli.timeout,
        )?,
        Mode::MuvmTrueMatrix => {
            let timeout_path = resolve_in_path("timeout").context("locate timeout in PATH")?;
            run_muvm_true_matrix(
                &muvm_path,
                &timeout_path,
                systemd_run_path.as_deref(),
                cli.systemd_tasks_max,
                &workdir_abs,
                cli.timeout,
                cli.matrix_runs,
            )?
        }
        Mode::Edge => {
            let _ = run_edge(
                &muvm_path,
                systemd_run_path.as_deref(),
                cli.systemd_tasks_max,
                &workdir_abs,
                &extracted_root_abs,
                cli.mem,
                cli.muvm_privileged,
                cli.strace,
                cli.strace_mode,
                Duration::from_secs(cli.timeout),
                Duration::from_secs(cli.edge_watchdog_seconds),
                &cli.url,
                cli.headless_impl,
                &cli.edge_arg,
                &cli.edge_env,
                cli.profile_location,
                cli.preserve_dbus_xdg_env,
                &cli.guest_sysctl,
            )?;
        }
        Mode::EdgeRepeat => run_edge_repeat(
            &muvm_path,
            systemd_run_path.as_deref(),
            cli.systemd_tasks_max,
            &workdir_abs,
            &extracted_root_abs,
            cli.mem,
            cli.muvm_privileged,
            cli.strace,
            cli.strace_mode,
            Duration::from_secs(cli.timeout),
            Duration::from_secs(cli.edge_watchdog_seconds),
            &cli.url,
            cli.headless_impl,
            &cli.edge_arg,
            &cli.edge_env,
            cli.profile_location,
            cli.preserve_dbus_xdg_env,
            &cli.guest_sysctl,
            cli.repeat_max_attempts,
            cli.repeat_stop_on,
        )?,
        Mode::AnalyzeRunDir => {
            let run_dir = cli
                .run_dir
                .as_deref()
                .context("--run-dir is required for --mode analyze-run-dir")?;
            run_analyze_run_dir(run_dir)?;
        }
        Mode::GuestRunner => unreachable!("handled above"),
    }

    eprintln!("Wrote log: {}", log_path.display());
    Ok(())
}

fn run_analyze_run_dir(run_dir: &Path) -> Result<()> {
    if !run_dir.is_dir() {
        bail!("run dir does not exist: {}", run_dir.display());
    }

    let stderr_path = run_dir.join("stderr.txt");
    if !stderr_path.is_file() {
        bail!("missing stderr.txt in run dir: {}", stderr_path.display());
    }

    let report_path = run_dir.join("pthread.stack-mprotect-enomem.txt");
    let analysis = analyze_pthread_stack_mprotect_enomem(run_dir, &stderr_path, &report_path)
        .context("analyze pthread stack mprotect ENOMEM")?;

    eprintln!("analysis_events_total: {}", analysis.events_total);
    eprintln!("wrote_report: {}", report_path.display());
    Ok(())
}

fn run_preflight(
    muvm_path: &Path,
    systemd_run_path: Option<&Path>,
    systemd_tasks_max: Option<u64>,
    workdir_abs: &Path,
    timeout_secs: u64,
) -> Result<()> {
    let run_dir = workdir_abs.join(format!("preflight-{}", chrono_stamp()));
    fs::create_dir_all(&run_dir).context("create preflight run dir")?;

    let muvm_output_path = run_dir.join("muvm.txt");
    let summary_path = run_dir.join("summary.txt");

    let args: Vec<String> = wrap_muvm_args_if_requested(
		vec![
			muvm_path.display().to_string(),
			"--emu=fex".into(),
			"-e".into(),
			format!("RUN_DIR={}", run_dir.display()),
			"bash".into(),
			"-lc".into(),
			"set -euo pipefail; echo \"hello\" >\"$RUN_DIR/vm-ok.txt\"; echo \"wrote:$RUN_DIR/vm-ok.txt\"".into(),
		],
		systemd_run_path,
		systemd_tasks_max,
	)?;

    let start = Instant::now();
    let rc =
        run_command_with_pty_to_file(&args, &muvm_output_path, Duration::from_secs(timeout_secs))
            .context("run muvm preflight")?;

    let ok_exists = run_dir.join("vm-ok.txt").is_file();

    let mut f = fs::File::create(&summary_path).context("write preflight summary")?;
    writeln!(f, "exit_code: {rc}")?;
    writeln!(f, "elapsed_seconds: {}", start.elapsed().as_secs())?;
    writeln!(f, "run_dir: {}", run_dir.display())?;
    writeln!(
        f,
        "systemd_tasks_max: {}",
        systemd_tasks_max
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(none)".to_string())
    )?;
    writeln!(f, "vm_ok_exists: {}", if ok_exists { "yes" } else { "no" })?;

    Ok(())
}

fn run_muvm_true(
    muvm_path: &Path,
    systemd_run_path: Option<&Path>,
    systemd_tasks_max: Option<u64>,
    workdir_abs: &Path,
    timeout_secs: u64,
) -> Result<()> {
    let run_dir = workdir_abs.join(format!("muvm-true-{}", chrono_stamp()));
    fs::create_dir_all(&run_dir).context("create muvm-true run dir")?;

    let muvm_output_path = run_dir.join("muvm.txt");
    let summary_path = run_dir.join("summary.txt");

    let args: Vec<String> = wrap_muvm_args_if_requested(
        vec![muvm_path.display().to_string(), "true".into()],
        systemd_run_path,
        systemd_tasks_max,
    )?;

    let start = Instant::now();
    let rc =
        run_command_with_pty_to_file(&args, &muvm_output_path, Duration::from_secs(timeout_secs))
            .context("run muvm true")?;

    let mut f = fs::File::create(&summary_path).context("write muvm-true summary")?;
    writeln!(f, "exit_code: {rc}")?;
    writeln!(f, "elapsed_seconds: {}", start.elapsed().as_secs())?;
    writeln!(f, "run_dir: {}", run_dir.display())?;
    writeln!(
        f,
        "systemd_tasks_max: {}",
        systemd_tasks_max
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(none)".to_string())
    )?;

    Ok(())
}

#[derive(Copy, Clone, Debug)]
enum StdioMode {
    Pty,
    InheritTty,
}

#[derive(Copy, Clone, Debug)]
enum KillMode {
    Internal,
    ExternalTimeout,
    ExternalTimeoutForeground,
}

fn run_muvm_true_matrix(
    muvm_path: &Path,
    timeout_path: &Path,
    systemd_run_path: Option<&Path>,
    systemd_tasks_max: Option<u64>,
    workdir_abs: &Path,
    timeout_secs: u64,
    runs_per_case: u32,
) -> Result<()> {
    let batch_dir = workdir_abs.join(format!("muvm-true-matrix-{}", chrono_stamp()));
    fs::create_dir_all(&batch_dir).context("create muvm-true matrix batch dir")?;
    let batch_summary_path = batch_dir.join("matrix-summary.txt");

    let cases: Vec<(StdioMode, KillMode, &'static str)> = vec![
        (StdioMode::Pty, KillMode::Internal, "pty/internal"),
        (StdioMode::Pty, KillMode::ExternalTimeout, "pty/timeout"),
        (StdioMode::InheritTty, KillMode::Internal, "tty/internal"),
        (
            StdioMode::InheritTty,
            KillMode::ExternalTimeout,
            "tty/timeout",
        ),
        (
            StdioMode::InheritTty,
            KillMode::ExternalTimeoutForeground,
            "tty/timeout-foreground",
        ),
    ];

    let mut batch_summary = String::new();
    batch_summary.push_str("# muvm true matrix\n");
    batch_summary.push_str(&format!("date: {}\n", iso_now()));
    batch_summary.push_str(&format!("timeout_secs: {timeout_secs}\n"));
    batch_summary.push_str(&format!("runs_per_case: {runs_per_case}\n"));
    batch_summary.push_str(&format!(
        "systemd_tasks_max: {}\n",
        systemd_tasks_max
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(none)".to_string())
    ));
    batch_summary.push_str("\n## runs\n");
    batch_summary.push_str("case\trun\texit\telapsed\ttimed_out\tstuck_snapshot\n");

    for (stdio_mode, kill_mode, case_name) in cases {
        for run_idx in 1..=runs_per_case {
            let run_dir = batch_dir.join(format!(
                "case-{}-run-{}-{}",
                case_name.replace('/', "_"),
                run_idx,
                chrono_stamp()
            ));
            fs::create_dir_all(&run_dir).context("create case run dir")?;

            let summary_path = run_dir.join("summary.txt");
            let output_path = run_dir.join("muvm.txt");
            let stuck_path = run_dir.join("stuck.txt");

            let argv: Vec<String>;
            let expected_kill_at = Duration::from_secs(timeout_secs);
            let snapshot_at = if matches!(
                kill_mode,
                KillMode::ExternalTimeout | KillMode::ExternalTimeoutForeground
            ) {
                Some(expected_kill_at.saturating_sub(Duration::from_secs(1)))
            } else {
                None
            };

            match kill_mode {
                KillMode::Internal => {
                    argv = wrap_muvm_args_if_requested(
                        vec![muvm_path.display().to_string(), "true".into()],
                        systemd_run_path,
                        systemd_tasks_max,
                    )?;
                }
                KillMode::ExternalTimeout => {
                    argv = wrap_muvm_args_if_requested(
                        vec![
                            timeout_path.display().to_string(),
                            format!("{timeout_secs}s"),
                            muvm_path.display().to_string(),
                            "true".into(),
                        ],
                        systemd_run_path,
                        systemd_tasks_max,
                    )?;
                }
                KillMode::ExternalTimeoutForeground => {
                    argv = wrap_muvm_args_if_requested(
                        vec![
                            timeout_path.display().to_string(),
                            "--foreground".into(),
                            format!("{timeout_secs}s"),
                            muvm_path.display().to_string(),
                            "true".into(),
                        ],
                        systemd_run_path,
                        systemd_tasks_max,
                    )?;
                }
            }

            let start = Instant::now();
            let (rc, timed_out) = match stdio_mode {
                StdioMode::Pty => {
                    let hook = |child_pid: libc::pid_t| {
                        let root = child_pid as u32;
                        let target = if matches!(
                            kill_mode,
                            KillMode::ExternalTimeout | KillMode::ExternalTimeoutForeground
                        ) {
                            find_vm_like_descendant_pid(root, 3, 64).unwrap_or(root)
                        } else {
                            root
                        };
                        write_stuck_snapshot_named(&stuck_path, target, "muvm").ok();
                    };

                    let timeout = if matches!(
                        kill_mode,
                        KillMode::ExternalTimeout | KillMode::ExternalTimeoutForeground
                    ) {
                        Duration::from_secs(timeout_secs + 5)
                    } else {
                        Duration::from_secs(timeout_secs)
                    };
                    let res = run_command_with_pty_to_file_observed(
                        &argv,
                        &output_path,
                        timeout,
                        snapshot_at,
                        &hook,
                    )
                    .context("run muvm matrix case (pty)")?;
                    (res.exit_code, res.timed_out)
                }
                StdioMode::InheritTty => {
                    let hook = |child_pid: libc::pid_t| {
                        let root = child_pid as u32;
                        let target = if matches!(
                            kill_mode,
                            KillMode::ExternalTimeout | KillMode::ExternalTimeoutForeground
                        ) {
                            find_vm_like_descendant_pid(root, 3, 64).unwrap_or(root)
                        } else {
                            root
                        };
                        write_stuck_snapshot_named(&stuck_path, target, "muvm").ok();
                    };

                    let timeout = if matches!(
                        kill_mode,
                        KillMode::ExternalTimeout | KillMode::ExternalTimeoutForeground
                    ) {
                        Duration::from_secs(timeout_secs + 5)
                    } else {
                        Duration::from_secs(timeout_secs)
                    };
                    let res = run_command_inherit_tty_observed(
                        &argv,
                        &output_path,
                        timeout,
                        snapshot_at,
                        &hook,
                    )
                    .context("run muvm matrix case (inherit tty)")?;
                    (res.exit_code, res.timed_out)
                }
            };

            let elapsed = start.elapsed().as_secs();
            let stuck_exists = stuck_path.is_file();

            let mut f = fs::File::create(&summary_path).context("write case summary")?;
            writeln!(f, "case: {case_name}")?;
            writeln!(f, "run: {run_idx}")?;
            writeln!(f, "stdio_mode: {:?}", stdio_mode)?;
            writeln!(f, "kill_mode: {:?}", kill_mode)?;
            writeln!(f, "exit_code: {rc}")?;
            writeln!(f, "elapsed_seconds: {elapsed}")?;
            writeln!(f, "timed_out: {}", if timed_out { "yes" } else { "no" })?;
            writeln!(
                f,
                "stuck_snapshot: {}",
                if stuck_exists { "yes" } else { "no" }
            )?;
            writeln!(f, "run_dir: {}", run_dir.display())?;
            writeln!(f, "output_log: {}", output_path.display())?;
            writeln!(f, "stuck_log: {}", stuck_path.display())?;

            batch_summary.push_str(&format!(
                "{case_name}\t{run_idx}\t{rc}\t{elapsed}\t{}\t{}\n",
                if timed_out { "yes" } else { "no" },
                if stuck_exists { "yes" } else { "no" }
            ));
        }
    }

    fs::write(&batch_summary_path, batch_summary).context("write matrix summary")?;
    eprintln!("Run dir: {}", batch_dir.display());
    Ok(())
}

#[derive(Debug, Clone)]
struct EdgeRunResult {
    run_dir: PathBuf,
    stdout_bytes: u64,
    stderr_pthread_create_lines: u64,
    pthread_stack_mprotect_enomem_events: u64,
}

fn run_edge(
    muvm_path: &Path,
    systemd_run_path: Option<&Path>,
    systemd_tasks_max: Option<u64>,
    workdir_abs: &Path,
    extracted_root_abs: &Path,
    mem: Option<u64>,
    muvm_privileged: bool,
    strace: bool,
    strace_mode: StraceMode,
    timeout: Duration,
    edge_watchdog: Duration,
    url: &str,
    headless_impl: HeadlessImpl,
    edge_args: &[String],
    edge_env: &[String],
    profile_location: ProfileLocation,
    preserve_dbus_xdg_env: bool,
    guest_sysctls: &[String],
) -> Result<EdgeRunResult> {
    if !extracted_root_abs.is_dir() {
        bail!(
            "No extracted root present; expected {}",
            extracted_root_abs.display()
        );
    }

    let edge_bin = extracted_root_abs.join("opt/microsoft/msedge/microsoft-edge");
    if !edge_bin.is_file() {
        bail!("Edge binary missing at {}", edge_bin.display());
    }

    let run_dir = workdir_abs.join(format!("headless-{}", chrono_stamp()));
    fs::create_dir_all(&run_dir).context("create run dir")?;
    if matches!(profile_location, ProfileLocation::Shared) {
        fs::create_dir_all(run_dir.join("profile")).context("create shared profile dir")?;
    }

    let stdout_path = run_dir.join("stdout.txt");
    let stderr_path = run_dir.join("stderr.txt");
    let stderr_filtered_path = run_dir.join("stderr.filtered.txt");
    let ps_path = run_dir.join("ps.txt");
    let threads_path = run_dir.join("threads.txt");
    let preflight_path = run_dir.join("preflight.txt");
    let summary_path = run_dir.join("summary.txt");
    let muvm_output_path = run_dir.join("muvm.txt");

    // Ensure the guest-runner binary is in a path that we know muvm shares.
    let self_exe = std::env::current_exe().context("locate current executable")?;
    let self_exe = fs::canonicalize(&self_exe).context("canonicalize current executable")?;
    let guest_runner_path = run_dir.join("edge-muvm-guest-runner");
    fs::copy(&self_exe, &guest_runner_path).context("copy guest-runner into run dir")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&guest_runner_path)
            .context("stat guest-runner")?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&guest_runner_path, perms).context("chmod guest-runner")?;
    }

    let mut args: Vec<String> = vec![muvm_path.display().to_string(), "--emu=fex".into()];
    if let Some(mem) = mem {
        args.push(format!("--mem={mem}"));
    }
    if muvm_privileged {
        args.push("--privileged".into());
    }

    if !preserve_dbus_xdg_env {
        // Avoid inheriting host DBus session env into a VM that doesn't have that bus.
        args.extend([
            "-e".into(),
            "DBUS_SESSION_BUS_ADDRESS=".into(),
            "-e".into(),
            "XDG_RUNTIME_DIR=".into(),
        ]);
    }

    args.push(guest_runner_path.display().to_string());
    args.push("--mode".into());
    args.push("guest-runner".into());
    args.push("--edge-bin".into());
    args.push(edge_bin.display().to_string());
    args.push("--run-dir".into());
    targs_push_path(&mut args, &run_dir);
    args.push("--url".into());
    args.push(url.to_string());
    args.push("--edge-watchdog-seconds".into());
    args.push(edge_watchdog.as_secs().to_string());
    args.push("--guest-headless-impl".into());
    args.push(match headless_impl {
        HeadlessImpl::New => "new".to_string(),
        HeadlessImpl::Old => "old".to_string(),
    });

    args.push("--profile-location".into());
    args.push(profile_location.as_arg().to_string());

    if preserve_dbus_xdg_env {
        args.push("--preserve-dbus-xdg-env".into());
    }

    for kv in guest_sysctls {
        args.push(format!("--guest-sysctl={kv}"));
    }

    for a in edge_args {
        args.push(format!("--edge-arg={a}"));
    }

    for kv in edge_env {
        args.push(format!("--edge-env={kv}"));
    }

    if strace {
        args.push("--strace".into());
        args.push("--strace-mode".into());
        args.push(match strace_mode {
            StraceMode::Minimal => "minimal".to_string(),
            StraceMode::Hang => "hang".to_string(),
        });
    }

    let args = wrap_muvm_args_if_requested(args, systemd_run_path, systemd_tasks_max)?;

    let start = Instant::now();
    let rc = run_command_with_pty_to_file(&args, &muvm_output_path, timeout).context("run muvm")?;

    if !stdout_path.is_file() || !stderr_path.is_file() {
        let mut f = fs::File::create(&summary_path).context("write missing-artifact summary")?;
        writeln!(f, "exit_code: {rc}")?;
        writeln!(f, "elapsed_seconds: {}", start.elapsed().as_secs())?;
        writeln!(f, "note: expected artifacts missing")?;
        writeln!(f, "run_dir: {}", run_dir.display())?;
        writeln!(f, "muvm_output: {}", muvm_output_path.display())?;
        return Ok(EdgeRunResult {
            run_dir,
            stdout_bytes: 0,
            stderr_pthread_create_lines: 0,
            pthread_stack_mprotect_enomem_events: 0,
        });
    }

    // Filter out crashpad/ptrace spam for quick review.
    filter_stderr(&stderr_path, &stderr_filtered_path).ok();

    let stdout_bytes = fs::metadata(&stdout_path).map(|m| m.len()).unwrap_or(0);
    let stderr_lines = count_lines(&stderr_path).unwrap_or(0);
    let ptrace_lines = count_substring_lines(&stderr_path, "ptrace:").unwrap_or(0);
    let pthread_lines = count_substring_lines(&stderr_path, "pthread_create").unwrap_or(0);
    let dbus_lines =
        count_substring_lines(&stderr_path, "Failed to connect to the bus").unwrap_or(0);
    let ssl_lines =
        count_substring_lines(&stderr_path, "ssl_client_socket_impl.cc:930").unwrap_or(0);
    let handshake_lines = count_substring_lines(&stderr_path, "handshake failed").unwrap_or(0);

    let pthread_stack_report_path = run_dir.join("pthread.stack-mprotect-enomem.txt");
    let pthread_analysis =
        analyze_pthread_stack_mprotect_enomem(&run_dir, &stderr_path, &pthread_stack_report_path)
            .unwrap_or_else(|_e| PthreadStackAnalysis {
                pthread_ids: Vec::new(),
                pthread_pids: Vec::new(),
                events_total: 0,
            });

    let preflight_kvs = extract_preflight_kvs(
        &preflight_path,
        &[
            "cgroup_v2_relative_path",
            "cgroup_v2_dir",
            "cgroup_v2_pids_max",
            "cgroup_v2_pids_current",
            "cgroup_v2_memory_max",
            "cgroup_v2_memory_current",
            "cgroup_v2_memory_high",
            "cgroup_v2_memory_events",
            "vm_overcommit_memory",
            "vm_overcommit_ratio",
            "vm_overcommit_kbytes",
            "vm_max_map_count",
        ],
    );

    let mut f = fs::File::create(&summary_path).context("write headless summary")?;
    writeln!(f, "exit_code: {rc}")?;
    writeln!(f, "elapsed_seconds: {}", start.elapsed().as_secs())?;
    writeln!(
        f,
        "systemd_tasks_max: {}",
        systemd_tasks_max
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(none)".to_string())
    )?;
    let edge_exit = fs::read_to_string(run_dir.join("edge-exit.txt"))
        .unwrap_or_else(|e| format!("(unavailable: {e})"));
    writeln!(f, "edge_exit: {}", edge_exit.trim())?;
    writeln!(
        f,
        "headless_impl: {}",
        match headless_impl {
            HeadlessImpl::New => "new",
            HeadlessImpl::Old => "old",
        }
    )?;
    writeln!(f, "stdout_bytes: {stdout_bytes}")?;
    writeln!(f, "stderr_lines: {stderr_lines}")?;
    writeln!(f, "stderr_ptrace_lines: {ptrace_lines}")?;
    writeln!(f, "stderr_pthread_create_lines: {pthread_lines}")?;
    writeln!(
        f,
        "pthread_stack_mprotect_enomem_events: {}",
        pthread_analysis.events_total
    )?;
    writeln!(
        f,
        "pthread_pids_from_stderr: {}",
        if pthread_analysis.pthread_pids.is_empty() {
            "(none)".to_string()
        } else {
            pthread_analysis
                .pthread_pids
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        }
    )?;
    writeln!(
        f,
        "pthread_ids_from_stderr: {}",
        if pthread_analysis.pthread_ids.is_empty() {
            "(none)".to_string()
        } else {
            pthread_analysis
                .pthread_ids
                .iter()
                .map(|(pid, tid)| format!("{pid}:{tid}"))
                .collect::<Vec<_>>()
                .join(" ")
        }
    )?;
    writeln!(f, "stderr_dbus_lines: {dbus_lines}")?;
    writeln!(f, "stderr_ssl_client_socket_lines: {ssl_lines}")?;
    writeln!(f, "stderr_handshake_failed_lines: {handshake_lines}")?;
    if !preflight_kvs.is_empty() {
        writeln!(f)?;
        writeln!(f, "preflight_kvs:")?;
        for (k, v) in preflight_kvs {
            writeln!(f, "  {k}: {v}")?;
        }
    }
    writeln!(f)?;
    writeln!(f, "artifacts:")?;
    writeln!(f, "  preflight: {}", preflight_path.display())?;
    writeln!(f, "  ps: {}", ps_path.display())?;
    writeln!(f, "  threads: {}", threads_path.display())?;
    writeln!(f, "  stdout: {}", stdout_path.display())?;
    writeln!(f, "  stderr: {}", stderr_path.display())?;
    writeln!(f, "  stderr_filtered: {}", stderr_filtered_path.display())?;
    writeln!(f, "  muvm: {}", muvm_output_path.display())?;
    writeln!(
        f,
        "  pthread_stack_report: {}",
        pthread_stack_report_path.display()
    )?;

    eprintln!("Run dir: {}", run_dir.display());
    Ok(EdgeRunResult {
        run_dir,
        stdout_bytes,
        stderr_pthread_create_lines: pthread_lines,
        pthread_stack_mprotect_enomem_events: pthread_analysis.events_total,
    })
}

fn extract_preflight_kvs(preflight_path: &Path, keys: &[&str]) -> Vec<(String, String)> {
    let Ok(s) = fs::read_to_string(preflight_path) else {
        return Vec::new();
    };
    let want: HashSet<&str> = keys.iter().copied().collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in s.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim();
        if !want.contains(k) {
            continue;
        }
        if !seen.insert(k.to_string()) {
            continue;
        }
        out.push((k.to_string(), v.trim().to_string()));
    }
    out
}

fn run_edge_repeat(
    muvm_path: &Path,
    systemd_run_path: Option<&Path>,
    systemd_tasks_max: Option<u64>,
    workdir_abs: &Path,
    extracted_root_abs: &Path,
    mem: Option<u64>,
    muvm_privileged: bool,
    strace: bool,
    strace_mode: StraceMode,
    timeout: Duration,
    edge_watchdog: Duration,
    url: &str,
    headless_impl: HeadlessImpl,
    edge_args: &[String],
    edge_env: &[String],
    profile_location: ProfileLocation,
    preserve_dbus_xdg_env: bool,
    guest_sysctls: &[String],
    max_attempts: u32,
    stop_on: RepeatStopOn,
) -> Result<()> {
    let repeat_log_path = workdir_abs.join(format!("edge-repeat-{}.txt", chrono_stamp()));
    let mut log = String::new();
    log.push_str(&format!("date: {}\n", iso_now()));
    log.push_str(&format!("max_attempts: {max_attempts}\n"));
    log.push_str(&format!("stop_on: {:?}\n", stop_on));
    log.push_str(&format!("strace: {}\n", if strace { "yes" } else { "no" }));
    log.push_str(&format!(
        "edge_watchdog_seconds: {}\n",
        edge_watchdog.as_secs()
    ));
    log.push_str(&format!("url: {url}\n"));
    log.push_str(&format!("headless_impl: {:?}\n", headless_impl));
    log.push_str(&format!(
        "mem: {}\n\n",
        mem.map(|m| m.to_string())
            .unwrap_or_else(|| "(none)".into())
    ));

    let mut hit: Option<EdgeRunResult> = None;
    let mut attempts = 0;
    for i in 1..=max_attempts {
        attempts = i;
        eprintln!("edge-repeat: attempt {i}/{max_attempts}");
        let res = run_edge(
            muvm_path,
            systemd_run_path,
            systemd_tasks_max,
            workdir_abs,
            extracted_root_abs,
            mem,
            muvm_privileged,
            strace,
            strace_mode,
            timeout,
            edge_watchdog,
            url,
            headless_impl,
            edge_args,
            edge_env,
            profile_location,
            preserve_dbus_xdg_env,
            guest_sysctls,
        )?;

        log.push_str(&format!(
            "attempt {i}: dir={} stdout_bytes={} pthread_lines={} stack_events={}\n",
            res.run_dir.display(),
            res.stdout_bytes,
            res.stderr_pthread_create_lines,
            res.pthread_stack_mprotect_enomem_events
        ));

        let should_stop = match stop_on {
            RepeatStopOn::PthreadCreate => res.stderr_pthread_create_lines > 0,
            RepeatStopOn::StackMprotectEnomem => res.pthread_stack_mprotect_enomem_events > 0,
            RepeatStopOn::StdoutNonEmpty => res.stdout_bytes > 0,
        };

        if should_stop {
            log.push_str(&format!(
                "\nstop: hit on attempt {i}: {}\n",
                res.run_dir.display()
            ));
            hit = Some(res);
            break;
        }
    }

    if hit.is_none() {
        log.push_str(&format!("\nstop: no hit after {attempts} attempts\n"));
    }

    fs::write(&repeat_log_path, log).context("write repeat log")?;

    if let Some(hit) = hit {
        eprintln!("edge-repeat: hit run dir: {}", hit.run_dir.display());
    } else {
        eprintln!("edge-repeat: no hit (see {})", repeat_log_path.display());
    }
    Ok(())
}

fn wrap_muvm_args_if_requested(
    argv: Vec<String>,
    systemd_run_path: Option<&Path>,
    systemd_tasks_max: Option<u64>,
) -> Result<Vec<String>> {
    let Some(tasks_max) = systemd_tasks_max else {
        return Ok(argv);
    };
    let systemd_run_path = systemd_run_path.context("--systemd-tasks-max requires systemd-run")?;

    let mut out = Vec::with_capacity(argv.len() + 8);
    out.push(systemd_run_path.display().to_string());
    out.push("--user".into());
    // Use a transient service (not a scope) so we can use --pty. This preserves
    // TTY/PTY semantics, which materially affects muvm/Edge behavior.
    out.push("--pty".into());
    out.push("--wait".into());
    out.push("--collect".into());
    out.push("-p".into());
    out.push(format!("TasksMax={tasks_max}"));
    out.push("--same-dir".into());
    out.push("--".into());
    out.extend(argv);
    Ok(out)
}

fn guest_runner(
    edge_bin: &Path,
    run_dir: &Path,
    url: &str,
    headless_impl: HeadlessImpl,
    edge_args: &[String],
    edge_env: &[String],
    profile_location: ProfileLocation,
    preserve_dbus_xdg_env: bool,
    guest_sysctls: &[String],
    strace: bool,
    strace_mode: StraceMode,
    edge_watchdog: Duration,
) -> Result<()> {
    if !edge_bin.is_file() {
        bail!("Edge binary missing at {}", edge_bin.display());
    }
    let profile_dir = match profile_location {
        ProfileLocation::Shared => run_dir.join("profile"),
        ProfileLocation::GuestTmp => {
            PathBuf::from(format!("/tmp/edge-muvm-profile-{}", chrono_stamp()))
        }
    };
    fs::create_dir_all(&profile_dir).context("create profile dir")?;

    let stdout_path = run_dir.join("stdout.txt");
    let stderr_path = run_dir.join("stderr.txt");
    let ps_path = run_dir.join("ps.txt");
    let threads_path = run_dir.join("threads.txt");
    let preflight_path = run_dir.join("preflight.txt");
    let pid_path = run_dir.join("pid.txt");
    let exit_path = run_dir.join("edge-exit.txt");
    let stuck_path = run_dir.join("stuck.txt");
    let guest_sysctl_path = run_dir.join("guest-sysctl.txt");

    {
        let mut f = fs::File::create(&preflight_path).context("write preflight")?;
        writeln!(f, "date: {}", iso_now())?;
        writeln!(f, "cwd: {}", std::env::current_dir()?.display())?;
        writeln!(f, "EDGE_BIN={}", edge_bin.display())?;
        writeln!(f, "RUN_DIR={}", run_dir.display())?;
        writeln!(f, "PROFILE_LOCATION={}", profile_location.as_arg())?;
        writeln!(f, "PROFILE_DIR={}", profile_dir.display())?;
        if !edge_args.is_empty() {
            writeln!(f, "EDGE_ARGS={}", edge_args.join(" "))?;
        }
        if !edge_env.is_empty() {
            writeln!(f, "EDGE_ENV={}", edge_env.join(" "))?;
        }
        writeln!(
            f,
            "PRESERVE_DBUS_XDG_ENV={}",
            if preserve_dbus_xdg_env { "yes" } else { "no" }
        )?;
        writeln!(
            f,
            "ENV_DBUS_SESSION_BUS_ADDRESS={}",
            std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_else(|_| "(unset)".into())
        )?;
        writeln!(
            f,
            "ENV_XDG_RUNTIME_DIR={}",
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "(unset)".into())
        )?;
        writeln!(f, "URL={}", url)?;
        writeln!(
            f,
            "HEADLESS_IMPL={}",
            match headless_impl {
                HeadlessImpl::New => "new",
                HeadlessImpl::Old => "old",
            }
        )?;
        writeln!(f, "EDGE_WATCHDOG_SECONDS={}", edge_watchdog.as_secs())?;
        writeln!(f)?;
        writeln!(f, "proc_self_status:")?;
        writeln!(
            f,
            "{}",
            read_text_best_effort(Path::new("/proc/self/status"), 256 * 1024)
        )?;
        writeln!(f)?;
        writeln!(f, "proc_self_cgroup:")?;
        let proc_self_cgroup = read_text_best_effort(Path::new("/proc/self/cgroup"), 64 * 1024);
        writeln!(f, "{proc_self_cgroup}")?;

        writeln!(f)?;
        writeln!(f, "effective_cgroup_v2:")?;
        if let Some(rel) = parse_cgroup_v2_relative_path(&proc_self_cgroup) {
            writeln!(f, "cgroup_v2_relative_path: {rel}")?;
            let dir = cgroup_v2_dir_from_relative_path(&rel);
            writeln!(f, "cgroup_v2_dir: {}", dir.display())?;

            // Machine-readable single-line keys for quick correlation.
            writeln!(
                f,
                "cgroup_v2_pids_max: {}",
                read_first_line_best_effort(&dir.join("pids.max"))
            )?;
            writeln!(
                f,
                "cgroup_v2_pids_current: {}",
                read_first_line_best_effort(&dir.join("pids.current"))
            )?;
            writeln!(
                f,
                "cgroup_v2_memory_max: {}",
                read_first_line_best_effort(&dir.join("memory.max"))
            )?;
            writeln!(
                f,
                "cgroup_v2_memory_current: {}",
                read_first_line_best_effort(&dir.join("memory.current"))
            )?;
            writeln!(
                f,
                "cgroup_v2_memory_high: {}",
                read_first_line_best_effort(&dir.join("memory.high"))
            )?;
            writeln!(
                f,
                "cgroup_v2_memory_events: {}",
                read_first_line_best_effort(&dir.join("memory.events"))
            )?;

            writeln!(f)?;
            writeln!(f, "cgroup_v2_files:")?;
            for file in [
                "pids.max",
                "pids.current",
                "pids.events",
                "memory.max",
                "memory.current",
                "memory.high",
                "memory.low",
                "memory.min",
                "memory.events",
                "memory.stat",
                "memory.swap.max",
                "memory.swap.current",
                "memory.oom.group",
                "cgroup.controllers",
                "cgroup.subtree_control",
                "cgroup.events",
                "cgroup.type",
            ] {
                let p = dir.join(file);
                let v = read_text_best_effort(&p, 256 * 1024);
                writeln!(f, "{}:\n{}", p.display(), v)?;
            }
        } else {
            writeln!(f, "(no unified cgroup v2 entry found in /proc/self/cgroup)")?;
        }
        writeln!(f)?;
        writeln!(f, "proc_self_mountinfo_cgroup_snippet:")?;
        writeln!(
            f,
            "{}",
            filter_lines(
                &read_text_best_effort(Path::new("/proc/self/mountinfo"), 512 * 1024),
                |l| l.contains("/sys/fs/cgroup")
            )
        )?;
        writeln!(f)?;
        writeln!(f, "kernel_threads_max:")?;
        writeln!(
            f,
            "{}",
            read_text_best_effort(Path::new("/proc/sys/kernel/threads-max"), 8 * 1024)
        )?;
        writeln!(f)?;
        writeln!(f, "kernel_pid_max:")?;
        writeln!(
            f,
            "{}",
            read_text_best_effort(Path::new("/proc/sys/kernel/pid_max"), 8 * 1024)
        )?;

        writeln!(f)?;
        writeln!(f, "vm_sysctls:")?;
        // Machine-readable single-line keys for quick correlation.
        writeln!(
            f,
            "vm_overcommit_memory: {}",
            read_first_line_best_effort(Path::new("/proc/sys/vm/overcommit_memory"))
        )?;
        writeln!(
            f,
            "vm_overcommit_ratio: {}",
            read_first_line_best_effort(Path::new("/proc/sys/vm/overcommit_ratio"))
        )?;
        writeln!(
            f,
            "vm_overcommit_kbytes: {}",
            read_first_line_best_effort(Path::new("/proc/sys/vm/overcommit_kbytes"))
        )?;
        writeln!(
            f,
            "vm_max_map_count: {}",
            read_first_line_best_effort(Path::new("/proc/sys/vm/max_map_count"))
        )?;

        // Full dumps for context.
        for p in [
            "/proc/sys/vm/overcommit_memory",
            "/proc/sys/vm/overcommit_ratio",
            "/proc/sys/vm/overcommit_kbytes",
            "/proc/sys/vm/max_map_count",
        ] {
            writeln!(f)?;
            writeln!(f, "{}:", p)?;
            writeln!(f, "{}", read_text_best_effort(Path::new(p), 8 * 1024))?;
        }
        writeln!(f)?;
        writeln!(f, "meminfo:")?;
        writeln!(
            f,
            "{}",
            read_text_best_effort(Path::new("/proc/meminfo"), 256 * 1024)
        )?;
        writeln!(f)?;
        writeln!(f, "proc_loadavg:")?;
        writeln!(
            f,
            "{}",
            read_text_best_effort(Path::new("/proc/loadavg"), 8 * 1024)
        )?;
        writeln!(f)?;
        writeln!(f, "cgroup_root_listing_ls_la:")?;
        writeln!(
            f,
            "{}",
            run_cmd_best_effort("ls", &["-la", "/sys/fs/cgroup"], 256 * 1024)
        )?;
        writeln!(f)?;
        writeln!(f, "cgroup_procs_count_and_sample:")?;
        writeln!(
            f,
            "{}",
            sample_and_count_lines(Path::new("/sys/fs/cgroup/cgroup.procs"), 20)
        )?;
        writeln!(f)?;
        writeln!(f, "cgroup_threads_count_and_sample:")?;
        writeln!(
            f,
            "{}",
            sample_and_count_lines(Path::new("/sys/fs/cgroup/cgroup.threads"), 20)
        )?;
        writeln!(f)?;
        writeln!(f, "ps_counts:")?;
        writeln!(f, "ps -e (lines): {}", run_cmd_count_lines("ps", &["-e"]))?;
        writeln!(
            f,
            "ps -eT (threads lines): {}",
            run_cmd_count_lines("ps", &["-eT"])
        )?;
        writeln!(
            f,
            "ps -eLf (tasks lines): {}",
            run_cmd_count_lines("ps", &["-eLf"])
        )?;
        writeln!(f)?;
        writeln!(f, "cgroup_pids_max_candidates:")?;
        for candidate in [
            "/sys/fs/cgroup/pids.max",
            "/sys/fs/cgroup/pids.current",
            "/sys/fs/cgroup/pids.events",
            "/sys/fs/cgroup/pids/pids.max",
            "/sys/fs/cgroup/pids/pids.current",
            "/sys/fs/cgroup/pids/pids.events",
            "/sys/fs/cgroup/cgroup.controllers",
            "/sys/fs/cgroup/cgroup.procs",
            "/sys/fs/cgroup/cgroup.threads",
            "/sys/fs/cgroup/cgroup.max.depth",
            "/sys/fs/cgroup/cgroup.max.descendants",
            "/sys/fs/cgroup/cgroup.subtree_control",
            "/sys/fs/cgroup/cgroup.events",
            "/sys/fs/cgroup/cgroup.type",
        ] {
            let p = Path::new(candidate);
            let v = read_text_best_effort(p, 64 * 1024);
            writeln!(f, "{candidate}:\n{v}")?;
        }
        if let Ok(limits) = fs::read_to_string("/proc/self/limits") {
            writeln!(f, "proc_self_limits:")?;
            writeln!(f, "{limits}")?;
        }
        writeln!(f, "ls_edge_bin:")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(edge_bin).context("stat edge bin")?;
            writeln!(f, "mode: {:o}", meta.permissions().mode())?;
        }
    }

    // Best-effort sysctl writes (log success/failure). Runs continue even if a write fails.
    if !guest_sysctls.is_empty() {
        let mut report = String::new();
        report.push_str(&format!("date: {}\n", iso_now()));
        for kv in guest_sysctls {
            let Some((k, v)) = kv.split_once('=') else {
                report.push_str(&format!(
                    "requested: {kv}\nresult: invalid (expected KEY=VALUE)\n\n"
                ));
                continue;
            };
            let k = k.trim();
            let v = v.trim();
            if k.is_empty() {
                report.push_str(&format!("requested: {kv}\nresult: invalid (empty key)\n\n"));
                continue;
            }
            if v.contains('\n') || v.contains('\r') {
                report.push_str(&format!(
                    "requested: {kv}\nresult: invalid (newline in value)\n\n"
                ));
                continue;
            }

            let mut valid = true;
            let mut prev_dot = false;
            for ch in k.chars() {
                let ok = ch.is_ascii_alphanumeric() || ch == '_' || ch == '.';
                if !ok {
                    valid = false;
                    break;
                }
                if ch == '.' {
                    if prev_dot {
                        valid = false;
                        break;
                    }
                    prev_dot = true;
                } else {
                    prev_dot = false;
                }
            }
            if k.starts_with('.') || k.ends_with('.') {
                valid = false;
            }
            if !valid {
                report.push_str(&format!("requested: {kv}\nresult: invalid (bad key)\n\n"));
                continue;
            }

            let path = PathBuf::from("/proc/sys").join(k.replace('.', "/"));
            let before = read_first_line_best_effort(&path);
            let write_res = fs::write(&path, format!("{v}\n"));
            let after = read_first_line_best_effort(&path);

            report.push_str(&format!(
                "requested: {k}={v}\npath: {}\nbefore: {before}\n",
                path.display()
            ));
            match write_res {
                Ok(_) => report.push_str("write: ok\n"),
                Err(e) => report.push_str(&format!("write: error: {e}\n")),
            }
            report.push_str(&format!("after: {after}\n\n"));
        }
        let _ = fs::write(&guest_sysctl_path, report);
    }

    let stdout_file = fs::File::create(&stdout_path).context("create stdout")?;
    let stderr_file = fs::File::create(&stderr_path).context("create stderr")?;

    // Optionally prefix Edge with strace.
    let strace_enabled_path = run_dir.join("strace.enabled.txt");
    let mut cmd = if strace {
        match resolve_in_path("strace") {
            Ok(p) => {
                let _ = fs::write(
                    &strace_enabled_path,
                    format!("strace: yes\npath: {}\n", p.display()),
                );
                let mut c = Command::new(p);
                let trace_set = match strace_mode {
                    StraceMode::Minimal => {
                        "clone,clone3,mmap,mprotect,munmap,mremap,brk,futex,prlimit64,setrlimit"
                    }
                    StraceMode::Hang => "process,signal,network,ipc,desc,memory",
                };
                // NOTE: `-s 0` makes string output useless (empty/abbreviated).
                // Use a moderate cap and `-v` so execve argv/etc. aren't shown as `[...]`.
                let strace_string_limit = match strace_mode {
                    StraceMode::Minimal => "128",
                    StraceMode::Hang => "256",
                };
                c.arg("-ff")
                    .arg("-tt")
                    .arg("-T")
                    .arg("-s")
                    .arg(strace_string_limit)
                    .arg("-v")
                    .arg("-o")
                    .arg(run_dir.join("strace"))
                    .arg("-e")
                    .arg(format!("trace={trace_set}"))
                    .arg(edge_bin);
                c
            }
            Err(e) => {
                let _ = fs::write(
                    &strace_enabled_path,
                    format!("strace: requested but not available ({e})\n"),
                );
                Command::new(edge_bin)
            }
        }
    } else {
        Command::new(edge_bin)
    };

    // Apply requested environment variables. This sets the env for the direct Edge process
    // and also works when wrapped in strace (Edge inherits strace's environment).
    for kv in edge_env {
        let Some((k, v)) = kv.split_once('=') else {
            bail!("invalid --edge-env value (expected KEY=VALUE): {kv}");
        };
        if k.is_empty() {
            bail!("invalid --edge-env value (empty KEY): {kv}");
        }
        cmd.env(k, v);
    }

    // Use newer headless implementation to avoid legacy headless limitations.
    let mut child = cmd
        .arg(match headless_impl {
            HeadlessImpl::New => "--headless",
            HeadlessImpl::Old => "--headless=old",
        })
        .arg("--disable-gpu")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        // Avoid keychain prompts during repeated headless runs.
        .arg("--password-store=basic")
        .arg("--use-mock-keychain")
        .arg("--disable-extensions")
        .arg("--disable-component-extensions-with-background-pages")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-breakpad")
        .arg("--disable-crash-reporter")
        .arg("--no-crash-upload")
        .arg("--disable-features=Crashpad")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .args(edge_args)
        .arg("--dump-dom")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
        .context("spawn Edge")?;

    let pid = child.id();

    // When wrapping Edge in `strace`, `child.id()` is the `strace` PID (not Edge).
    // For artifacts (ps/threads/stuck), we want the actual Edge/browser PID.
    let wrapper_pid = pid;
    let tracked_pid = if strace {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(2);
        let mut edge_pid = None;
        while Instant::now() < deadline {
            if let Ok(children) = pids_by_ppid(wrapper_pid) {
                if let Some(p) = children.first().copied() {
                    edge_pid = Some(p);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        edge_pid.unwrap_or(wrapper_pid)
    } else {
        wrapper_pid
    };

    let _ = fs::write(
        &pid_path,
        format!(
            "wrapper_pid={wrapper_pid}\ntracked_pid={tracked_pid}\nwrapped_in_strace={}\n",
            if strace { "yes" } else { "no" }
        ),
    );

    // Wait for a bounded time for Edge to finish dumping the DOM.
    let deadline = Instant::now() + edge_watchdog;
    let mut status = None;
    while Instant::now() < deadline {
        if let Some(s) = child.try_wait().context("poll Edge")? {
            status = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    write_ps(&ps_path, tracked_pid).ok();
    write_threads(&threads_path, tracked_pid).ok();

    if status.is_none() {
        // Capture a best-effort snapshot of what the process is doing before we kill it.
        write_stuck_snapshot(&stuck_path, tracked_pid).ok();

        // Keep runs bounded.
        // Kill the strace wrapper's process tree to ensure Edge (and any children)
        // are terminated as well.
        #[cfg(unix)]
        {
            kill_process_tree(wrapper_pid, libc::SIGKILL, 4096);
        }
        let _ = child.kill();
        status = child.wait().ok();
    }

    let mut f = fs::File::create(&exit_path).context("write edge exit")?;
    writeln!(
        f,
        "edge_exit: {}",
        status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    )?;
    Ok(())
}

fn parse_cgroup_v2_relative_path(proc_self_cgroup: &str) -> Option<String> {
    // cgroup v2 line format: 0::/some/path
    for line in proc_self_cgroup.lines() {
        if let Some(rest) = line.strip_prefix("0::") {
            let rel = rest.trim();
            if rel.is_empty() {
                return None;
            }
            return Some(rel.to_string());
        }
    }
    None
}

fn cgroup_v2_dir_from_relative_path(rel: &str) -> PathBuf {
    // rel is typically like "/user.slice/..." or "/".
    if rel == "/" {
        return PathBuf::from("/sys/fs/cgroup");
    }
    let rel = rel.trim_start_matches('/');
    PathBuf::from("/sys/fs/cgroup").join(rel)
}

fn read_first_line_best_effort(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(s) => s.lines().next().unwrap_or("").trim().to_string(),
        Err(e) => format!("(unavailable: {e})"),
    }
}

fn read_text_best_effort(path: &Path, max_bytes: usize) -> String {
    match fs::read(path) {
        Ok(bytes) => {
            let clipped = if bytes.len() > max_bytes {
                &bytes[..max_bytes]
            } else {
                &bytes[..]
            };
            let mut s = String::from_utf8_lossy(clipped).to_string();
            if bytes.len() > max_bytes {
                s.push_str("\n…(clipped)…\n");
            }
            s
        }
        Err(e) => format!("(unavailable: {e})"),
    }
}

fn filter_lines(input: &str, keep: impl Fn(&str) -> bool) -> String {
    let mut out = String::new();
    for line in input.lines() {
        if keep(line) {
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        "(no matches)\n".to_string()
    } else {
        out
    }
}

#[derive(Debug, Clone)]
struct PthreadStackAnalysis {
    pthread_ids: Vec<(u32, u32)>,
    pthread_pids: Vec<u32>,
    events_total: u64,
}

fn parse_bracket_pid_tid(line: &str) -> Option<(u32, u32)> {
    // Chromium logs often prefix as: [PID:TID:...]
    // We only care about the first pid:tid pair.
    let start = line.find('[')?;
    let s = &line[start + 1..];
    let mut it = s.chars().peekable();

    let mut pid: u32 = 0;
    let mut saw_pid = false;
    while let Some(c) = it.peek().copied() {
        if c.is_ascii_digit() {
            saw_pid = true;
            pid = pid
                .saturating_mul(10)
                .saturating_add((c as u8 - b'0') as u32);
            it.next();
        } else {
            break;
        }
    }
    if !saw_pid {
        return None;
    }
    if it.next()? != ':' {
        return None;
    }

    let mut tid: u32 = 0;
    let mut saw_tid = false;
    while let Some(c) = it.peek().copied() {
        if c.is_ascii_digit() {
            saw_tid = true;
            tid = tid
                .saturating_mul(10)
                .saturating_add((c as u8 - b'0') as u32);
            it.next();
        } else {
            break;
        }
    }
    if !saw_tid {
        return None;
    }
    Some((pid, tid))
}

fn unique_pids(ids: &[(u32, u32)]) -> Vec<u32> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (pid, _tid) in ids {
        if seen.insert(*pid) {
            out.push(*pid);
        }
    }
    out
}

fn pick_strace_path(run_dir: &Path, pid: u32, tid: u32) -> Option<(PathBuf, String)> {
    // Prefer thread ID (strace -ff usually keys files by tid), but keep compatibility
    // with either `strace.<pid>` or host-side `host.strace.<pid>`.
    let candidates: [(u32, &str); 2] = [(tid, "tid"), (pid, "pid")];
    for (ident, kind) in candidates {
        for prefix in ["strace.", "host.strace."] {
            let p = run_dir.join(format!("{prefix}{ident}"));
            if p.is_file() {
                return Some((p, format!("matched {kind}={ident}")));
            }
        }
    }
    None
}

fn extract_hex_after_equals(line: &str) -> Option<String> {
    // Example: mmap(...)= 0x7fffdfea0000
    let eq = line.rfind("=")?;
    let tail = line[eq + 1..].trim_start();
    if !tail.starts_with("0x") {
        return None;
    }
    let mut end = 2;
    for c in tail[2..].chars() {
        if c.is_ascii_hexdigit() {
            end += c.len_utf8();
        } else {
            break;
        }
    }
    Some(tail[..end].to_string())
}

fn analyze_pthread_stack_mprotect_enomem(
    run_dir: &Path,
    stderr_path: &Path,
    report_path: &Path,
) -> Result<PthreadStackAnalysis> {
    let stderr = fs::read_to_string(stderr_path).unwrap_or_default();
    let mut ids: Vec<(u32, u32)> = Vec::new();
    let mut seen = HashSet::new();
    for line in stderr.lines() {
        if !line.contains("pthread_create") {
            continue;
        }
        if let Some((pid, tid)) = parse_bracket_pid_tid(line) {
            if seen.insert((pid, tid)) {
                ids.push((pid, tid));
            }
        }
    }
    let pids = unique_pids(&ids);

    fn parse_u64_hex(s: &str) -> Option<u64> {
        let t = s.trim();
        let t = t.strip_prefix("0x").unwrap_or(t);
        u64::from_str_radix(t, 16).ok()
    }

    fn parse_u64_dec(s: &str) -> Option<u64> {
        s.trim().parse::<u64>().ok()
    }

    fn parse_syscall_args<'a>(line: &'a str, name: &str) -> Option<Vec<&'a str>> {
        let needle = format!("{name}(");
        let start = line.find(&needle)? + needle.len();
        let rest = &line[start..];
        let end = rest.find(')')?;
        let inside = &rest[..end];
        Some(inside.split(',').map(|p| p.trim()).collect())
    }

    fn parse_strace_mmap_stack(line: &str) -> Option<(u64, u64)> {
        if !line.contains("mmap(") || !line.contains("MAP_STACK") {
            return None;
        }
        let args = parse_syscall_args(line, "mmap")?;
        // mmap(addr, length, prot, flags, fd, offset)
        let len = parse_u64_dec(args.get(1)?)?;
        let base = extract_hex_after_equals(line).and_then(|h| parse_u64_hex(&h))?;
        Some((base, len))
    }

    fn parse_strace_mprotect_enomem(line: &str) -> Option<(u64, u64)> {
        if !line.contains("mprotect(") {
            return None;
        }
        if !line.contains("PROT_READ|PROT_WRITE") || !line.contains("= -1 ENOMEM") {
            return None;
        }
        let args = parse_syscall_args(line, "mprotect")?;
        // mprotect(addr, len, prot)
        let addr = parse_u64_hex(args.get(0)?)?;
        let len = parse_u64_dec(args.get(1)?)?;
        Some((addr, len))
    }

    let mut report = String::new();
    report.push_str("pthread_ids_from_stderr: ");
    if ids.is_empty() {
        report.push_str("(none)\n");
    } else {
        report.push_str(
            &ids.iter()
                .map(|(pid, tid)| format!("{pid}:{tid}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
        report.push('\n');
    }
    report.push_str("pthread_pids_from_stderr: ");
    if pids.is_empty() {
        report.push_str("(none)\n");
    } else {
        report.push_str(
            &pids
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" "),
        );
        report.push('\n');
    }

    let mut events_total: u64 = 0;
    for (pid, tid) in &ids {
        report.push_str(&format!("\n== pid {pid} tid {tid} ==\n"));
        let Some((strace_path, match_note)) = pick_strace_path(run_dir, *pid, *tid) else {
            report.push_str("strace: (missing)\n");
            continue;
        };
        report.push_str(&format!(
            "strace: {} ({match_note})\n",
            strace_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));

        let text = fs::read_to_string(&strace_path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let mut pid_events: u64 = 0;

        for (i, line) in lines.iter().enumerate() {
            let Some((mmap_base, mmap_len)) = parse_strace_mmap_stack(line) else {
                continue;
            };
            let mmap_end = mmap_base.saturating_add(mmap_len);

            let end = (i + 250).min(lines.len());
            for j in (i + 1)..end {
                let l = lines[j];
                let Some((mp_addr, mp_len)) = parse_strace_mprotect_enomem(l) else {
                    continue;
                };
                let mp_end = mp_addr.saturating_add(mp_len);

                // Typical stack setup: mmap(PROT_NONE, MAP_STACK) returns base,
                // then mprotect(base + page_size, len - page_size, RW) to leave a guard page.
                // Don't require exact base address match; accept any mprotect range that falls
                // within the mapping.
                let within_mapping = mp_addr >= mmap_base && mp_end <= mmap_end;
                let page_size: u64 = 4096;
                let guard_page_shape = mp_addr == mmap_base.saturating_add(page_size)
                    && (mp_len == mmap_len.saturating_sub(page_size)
                        || mp_len == mmap_len.saturating_sub(page_size * 2));

                if within_mapping || guard_page_shape {
                    pid_events += 1;
                    events_total += 1;
                    report.push_str(&format!(
                        "\n-- stack mprotect ENOMEM event #{pid_events} --\n"
                    ));
                    report.push_str(&format!(
                        "mmap_base: 0x{mmap_base:x} mmap_len: {mmap_len} mmap_end: 0x{mmap_end:x}\n"
                    ));
                    report.push_str(&format!(
                        "mprotect_addr: 0x{mp_addr:x} mprotect_len: {mp_len} mprotect_end: 0x{mp_end:x}\n"
                    ));

                    let lo = j.saturating_sub(5);
                    let hi = (j + 4).min(lines.len());
                    for ctx in &lines[lo..hi] {
                        report.push_str(ctx);
                        report.push('\n');
                    }
                    break;
                }
            }
        }

        report.push_str(&format!("stack_mprotect_enomem_events: {pid_events}\n"));
    }

    report.push_str(&format!(
        "\nstack_mprotect_enomem_events_total: {events_total}\n"
    ));

    fs::write(report_path, report).context("write pthread stack report")?;

    Ok(PthreadStackAnalysis {
        pthread_ids: ids,
        pthread_pids: pids,
        events_total,
    })
}

fn run_cmd_best_effort(program: &str, args: &[&str], max_bytes: usize) -> String {
    let output = Command::new(program).args(args).output();
    match output {
        Ok(out) => {
            let mut buf = Vec::new();
            buf.extend_from_slice(&out.stdout);
            if !out.stderr.is_empty() {
                buf.extend_from_slice(b"\n--- stderr ---\n");
                buf.extend_from_slice(&out.stderr);
            }
            if buf.is_empty() {
                return "(no output)".to_string();
            }
            let clipped = if buf.len() > max_bytes {
                &buf[..max_bytes]
            } else {
                &buf[..]
            };
            let mut s = String::from_utf8_lossy(clipped).to_string();
            if buf.len() > max_bytes {
                s.push_str("\n…(clipped)…\n");
            }
            s
        }
        Err(e) => format!("(failed to run {program}: {e})"),
    }
}

fn run_cmd_count_lines(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output();
    match output {
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout);
            let n = s.lines().count();
            format!("{n}")
        }
        Err(e) => format!("(failed: {e})"),
    }
}

fn sample_and_count_lines(path: &Path, sample: usize) -> String {
    match fs::read_to_string(path) {
        Ok(s) => {
            let lines: Vec<&str> = s.lines().collect();
            let mut out = String::new();
            out.push_str(&format!("count: {}\n", lines.len()));
            out.push_str("sample:\n");
            for l in lines.into_iter().take(sample) {
                out.push_str(l);
                out.push('\n');
            }
            out
        }
        Err(e) => format!("(unavailable: {e})"),
    }
}

fn write_stuck_snapshot(path: &Path, pid: u32) -> Result<()> {
    write_stuck_snapshot_named(path, pid, "edge")
}

fn write_stuck_snapshot_named(path: &Path, pid: u32, label: &str) -> Result<()> {
    let mut out = String::new();
    out.push_str("### stuck snapshot\n");
    out.push_str(&format!("pid: {pid}\n"));
    out.push_str(&format!("date: {}\n\n", iso_now()));

    // Time series: take two close snapshots to distinguish "stuck but progressing" from
    // "stuck and stationary" without ptrace.
    let ppoll_pipe_inodes_t0 = collect_ppoll_eventfd_pipe_inodes(pid, 24);
    let writer_pids_t0 = collect_pipe_writer_pids(&ppoll_pipe_inodes_t0, 512, 256, 10);
    let mut writer_sig_t0: HashMap<u32, TaskSignature> = HashMap::new();
    for wp in writer_pids_t0.iter().copied().take(6) {
        if let Some(sig) = sample_task_signature(wp, 12) {
            writer_sig_t0.insert(wp, sig);
        }
    }

    snapshot_proc(&mut out, pid, &format!("{label}_t0"));
    let parent_pid = read_parent_pid(pid).filter(|ppid| *ppid > 1 && *ppid != pid);
    if let Some(ppid) = parent_pid {
        out.push_str(&format!("\n--- {label}_parent (ppid={ppid}) ---\n"));
        snapshot_proc(&mut out, ppid, &format!("{label}_parent"));
    }

    // Compact, side-by-side view for upstream/debugging: shows whether the target and its
    // wrapper (parent) are in the terminal's foreground process group.
    out.push_str(&format!("\n[{label}] job_control_compare\n"));
    append_job_control_compare(&mut out, pid, parent_pid);
    out.push_str(&format!("\n--- {label}_timeseries_sleep_ms: 250 ---\n"));
    std::thread::sleep(Duration::from_millis(250));
    snapshot_proc(&mut out, pid, &format!("{label}_t1"));

    // After t1 snapshot, emit a compact diff-like summary for the writer PIDs we identified at t0.
    if !writer_pids_t0.is_empty() {
        out.push_str(&format!(
            "\n[{label}_timeseries] writer_pid_progress (t0 -> t1)\n"
        ));
        out.push_str("writer_pid_progress:\n");
        for wp in writer_pids_t0.iter().copied().take(6) {
            let Some(t0) = writer_sig_t0.get(&wp) else {
                continue;
            };
            let t1 = sample_task_signature(wp, 12);
            match t1 {
                None => {
                    out.push_str(&format!(
                        "  pid={wp} changed=(unknown) note=missing_or_unreadable\n"
                    ));
                }
                Some(t1) => {
                    let changed = if t0.digest != t1.digest || t0.leader_wchan != t1.leader_wchan {
                        "yes"
                    } else {
                        "no"
                    };
                    out.push_str(&format!("  pid={wp} changed={changed}\n"));
                    out.push_str(&format!(
						"    leader: t0_wchan={} t0_syscall_nr={} -> t1_wchan={} t1_syscall_nr={}\n",
						t0.leader_wchan,
						t0.leader_syscall_nr
							.map(|n| n.to_string())
							.unwrap_or_else(|| "?".to_string()),
						t1.leader_wchan,
						t1.leader_syscall_nr
							.map(|n| n.to_string())
							.unwrap_or_else(|| "?".to_string())
					));
                    out.push_str(&format!(
                        "    tasks: t0_count={} t1_count={}\n",
                        t0.task_count, t1.task_count
                    ));
                }
            }
        }
    }

    // Also snapshot a few direct children, if any.
    if let Ok(children) = pids_by_ppid(pid) {
        for (i, child_pid) in children.into_iter().take(3).enumerate() {
            out.push_str(&format!("\n--- child[{i}] ---\n"));
            snapshot_proc(&mut out, child_pid, "child");
        }
    }

    fs::write(path, out).context("write stuck snapshot")
}

struct ObservedRun {
    exit_code: i32,
    timed_out: bool,
}

fn run_command_inherit_tty_observed(
    args: &[String],
    log_path: &Path,
    timeout: Duration,
    snapshot_at: Option<Duration>,
    on_snapshot: &dyn Fn(libc::pid_t),
) -> Result<ObservedRun> {
    if args.is_empty() {
        bail!("no command provided");
    }

    // We can't safely capture/tee TTY output without adding wrappers that change behavior.
    // Instead, emit a small harness log noting that output was inherited.
    let mut log =
        fs::File::create(log_path).with_context(|| format!("create {}", log_path.display()))?;
    writeln!(log, "[inherit-tty] argv: {:?}", args)?;
    writeln!(
        log,
        "[inherit-tty] note: child output is inherited by the parent terminal"
    )?;

    let cstr_args: Vec<CString> = args
        .iter()
        .map(|s| CString::new(s.as_bytes()).context("NUL in arg"))
        .collect::<Result<_>>()?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!("fork failed: {}", io::Error::last_os_error());
    }

    if pid == 0 {
        unsafe {
            let mut argv: Vec<*const libc::c_char> = cstr_args.iter().map(|s| s.as_ptr()).collect();
            argv.push(std::ptr::null());
            libc::execvp(argv[0], argv.as_ptr());
            libc::_exit(127);
        }
    }

    let start = Instant::now();
    let mut did_snapshot = false;
    let mut timed_out = false;
    let exit_code;
    loop {
        if let Ok(Some(code)) = waitpid_nonblocking(pid) {
            exit_code = code;
            break;
        }

        let elapsed = start.elapsed();
        if !did_snapshot {
            if let Some(at) = snapshot_at {
                if elapsed >= at {
                    on_snapshot(pid);
                    did_snapshot = true;
                }
            }
        }

        if elapsed >= timeout {
            timed_out = true;
            on_snapshot(pid);
            kill_process_tree(pid as u32, libc::SIGTERM, 2048);
            let grace_start = Instant::now();
            let mut code: Option<i32> = None;
            while grace_start.elapsed() < Duration::from_millis(500) {
                if let Ok(Some(c)) = waitpid_nonblocking(pid) {
                    code = Some(c);
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if code.is_none() {
                kill_process_tree(pid as u32, libc::SIGKILL, 2048);
                code = waitpid_blocking(pid).ok();
            }
            exit_code = code.unwrap_or(124);
            break;
        }

        std::thread::sleep(Duration::from_millis(20));
    }

    Ok(ObservedRun {
        exit_code,
        timed_out,
    })
}

fn run_command_with_pty_to_file_observed(
    args: &[String],
    log_path: &Path,
    timeout: Duration,
    snapshot_at: Option<Duration>,
    on_snapshot: &dyn Fn(libc::pid_t),
) -> Result<ObservedRun> {
    if args.is_empty() {
        bail!("no command provided");
    }

    let cstr_args: Vec<CString> = args
        .iter()
        .map(|s| CString::new(s.as_bytes()).context("NUL in arg"))
        .collect::<Result<_>>()?;

    // PTY setup
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
    if master < 0 {
        bail!("posix_openpt failed: {}", io::Error::last_os_error());
    }
    unsafe {
        if libc::grantpt(master) != 0 {
            let e = io::Error::last_os_error();
            libc::close(master);
            bail!("grantpt failed: {e}");
        }
        if libc::unlockpt(master) != 0 {
            let e = io::Error::last_os_error();
            libc::close(master);
            bail!("unlockpt failed: {e}");
        }
    }

    set_nonblocking(master).context("set pty master nonblocking")?;
    let slave_name = ptsname(master).context("ptsname")?;

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let e = io::Error::last_os_error();
        unsafe { libc::close(master) };
        bail!("fork failed: {e}");
    }

    if pid == 0 {
        // Child
        unsafe {
            // New session so we can acquire a controlling terminal.
            if libc::setsid() < 0 {
                child_fail(master, "setsid", io::Error::last_os_error());
            }

            let slave = libc::open(slave_name.as_ptr(), libc::O_RDWR);
            if slave < 0 {
                child_fail(master, "open(slave)", io::Error::last_os_error());
            }

            // Make the PTY the controlling terminal.
            if libc::ioctl(slave, libc::TIOCSCTTY, 0) < 0 {
                child_fail(master, "ioctl(TIOCSCTTY)", io::Error::last_os_error());
            }

            // Hook up stdio.
            if libc::dup2(slave, 0) < 0 {
                child_fail(master, "dup2(stdin)", io::Error::last_os_error());
            }
            if libc::dup2(slave, 1) < 0 {
                child_fail(master, "dup2(stdout)", io::Error::last_os_error());
            }
            if libc::dup2(slave, 2) < 0 {
                child_fail(master, "dup2(stderr)", io::Error::last_os_error());
            }

            // Close fds we no longer need.
            libc::close(master);
            if slave > 2 {
                libc::close(slave);
            }

            // Build argv for execvp
            let mut argv: Vec<*const libc::c_char> = cstr_args.iter().map(|s| s.as_ptr()).collect();
            argv.push(std::ptr::null());

            libc::execvp(argv[0], argv.as_ptr());
            child_fail(master, "execvp", io::Error::last_os_error());
        }
    }

    // Parent
    let mut log =
        fs::File::create(log_path).with_context(|| format!("create {}", log_path.display()))?;

    let start = Instant::now();
    let mut exit_code: Option<i32> = None;
    let mut did_snapshot = false;
    let mut timed_out = false;

    loop {
        // Drain any PTY output.
        drain_master(master, &mut log).ok();

        // Check child exit.
        match waitpid_nonblocking(pid) {
            Ok(Some(code)) => {
                exit_code = Some(code);
                break;
            }
            Ok(None) => {}
            Err(e) => {
                unsafe { libc::close(master) };
                return Err(e).context("waitpid")?;
            }
        }

        let elapsed = start.elapsed();
        if !did_snapshot {
            if let Some(at) = snapshot_at {
                if elapsed >= at {
                    on_snapshot(pid);
                    did_snapshot = true;
                }
            }
        }

        if elapsed >= timeout {
            timed_out = true;
            on_snapshot(pid);
            // Graceful stop, then hard kill.
            kill_process_group(pid, libc::SIGTERM);
            // Brief grace window.
            let grace_start = Instant::now();
            while grace_start.elapsed() < Duration::from_millis(500) {
                drain_master(master, &mut log).ok();
                if let Ok(Some(code)) = waitpid_nonblocking(pid) {
                    exit_code = Some(code);
                    break;
                }
            }
            if exit_code.is_none() {
                kill_process_group(pid, libc::SIGKILL);
                let _ = waitpid_blocking(pid).map(|c| exit_code = Some(c));
            }
            break;
        }

        std::thread::sleep(Duration::from_millis(20));
    }

    // Final drain.
    drain_master(master, &mut log).ok();

    unsafe { libc::close(master) };
    Ok(ObservedRun {
        exit_code: exit_code.unwrap_or(124),
        timed_out,
    })
}

#[derive(Clone, Debug)]
struct TaskSignature {
    task_count: usize,
    digest: u64,
    leader_wchan: String,
    leader_syscall_nr: Option<u64>,
}

fn collect_ppoll_eventfd_pipe_inodes(pid: u32, max_tasks: usize) -> Vec<u64> {
    let task_dir = PathBuf::from(format!("/proc/{pid}/task"));
    let entries = match fs::read_dir(&task_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut tids: Vec<u32> = Vec::new();
    for ent in entries.flatten() {
        let s = ent.file_name().to_string_lossy().to_string();
        if let Ok(tid) = s.parse::<u32>() {
            tids.push(tid);
        }
    }
    tids.sort_unstable();

    let mut out: Vec<u64> = Vec::new();
    for tid in tids.into_iter().take(max_tasks) {
        let syscall = read_text_best_effort(&task_dir.join(format!("{tid}/syscall")), 4096)
            .trim()
            .to_string();
        let Some(sc) = parse_proc_syscall_line(&syscall) else {
            continue;
        };
        if sc.nr != 73 {
            continue;
        }
        let pollfd_ptr = sc.args[0];
        let nfds = sc.args[1] as usize;
        if !(1..=8).contains(&nfds) {
            continue;
        }

        let mut pollfds: Vec<libc::pollfd> = vec![unsafe { std::mem::zeroed() }; nfds];
        if read_remote_pollfds(pid, pollfd_ptr, &mut pollfds).is_err() {
            continue;
        }

        let mut has_eventfd = false;
        let mut pipe_inodes: Vec<u64> = Vec::new();
        for pfd in pollfds.iter() {
            let fd = pfd.fd;
            if fd < 0 {
                continue;
            }
            let target = read_fd_target(pid, fd as u32);
            if target.contains("anon_inode:[eventfd]") {
                has_eventfd = true;
            }
            if let Some(inode) = parse_pipe_inode(&target) {
                pipe_inodes.push(inode);
            }
        }
        if has_eventfd {
            out.extend(pipe_inodes);
        }
    }

    out.sort_unstable();
    out.dedup();
    out
}

fn collect_pipe_writer_pids(
    pipe_inodes: &[u64],
    max_pids: usize,
    max_fds_per_pid: usize,
    max_hits_per_inode: usize,
) -> Vec<u32> {
    let wanted: HashSet<u64> = pipe_inodes.iter().copied().collect();
    if wanted.is_empty() {
        return Vec::new();
    }

    let proc_entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut hit_counts: HashMap<u64, usize> = HashMap::new();
    for inode in &wanted {
        hit_counts.insert(*inode, 0);
    }

    let mut scanned_pids = 0usize;
    let mut writer_pids: Vec<u32> = Vec::new();
    for ent in proc_entries.flatten() {
        if scanned_pids >= max_pids {
            break;
        }
        if hit_counts.values().all(|c| *c >= max_hits_per_inode) {
            break;
        }

        let s = ent.file_name().to_string_lossy().to_string();
        let Ok(other_pid) = s.parse::<u32>() else {
            continue;
        };
        scanned_pids += 1;

        let fd_dir = PathBuf::from(format!("/proc/{other_pid}/fd"));
        let fds = match fs::read_dir(&fd_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let mut scanned_fds = 0usize;
        for fd_ent in fds.flatten() {
            if scanned_fds >= max_fds_per_pid {
                break;
            }
            scanned_fds += 1;
            let fd_name = fd_ent.file_name().to_string_lossy().to_string();
            let Ok(fd_num) = fd_name.parse::<u32>() else {
                continue;
            };
            let target = match fs::read_link(fd_dir.join(fd_num.to_string())) {
                Ok(t) => t.display().to_string(),
                Err(_) => continue,
            };
            let Some(inode) = parse_pipe_inode(&target) else {
                continue;
            };
            if !wanted.contains(&inode) {
                continue;
            }
            let count = hit_counts.entry(inode).or_insert(0);
            if *count >= max_hits_per_inode {
                continue;
            }

            let fdinfo_path = PathBuf::from(format!("/proc/{other_pid}/fdinfo/{fd_num}"));
            let fdinfo = read_text_best_effort(&fdinfo_path, 8 * 1024);
            let Some(flags) = parse_fdinfo_flags(&fdinfo) else {
                continue;
            };
            let access = access_mode_from_open_flags(flags);
            if access == "wronly" || access == "rdwr" {
                writer_pids.push(other_pid);
            }
            *count += 1;
        }
    }

    writer_pids.sort_unstable();
    writer_pids.dedup();
    writer_pids
}

fn sample_task_signature(pid: u32, max_tasks: usize) -> Option<TaskSignature> {
    use std::hash::{Hash, Hasher};

    let task_dir = PathBuf::from(format!("/proc/{pid}/task"));
    let entries = fs::read_dir(&task_dir).ok()?;
    let mut tids: Vec<u32> = Vec::new();
    for ent in entries.flatten() {
        let s = ent.file_name().to_string_lossy().to_string();
        if let Ok(tid) = s.parse::<u32>() {
            tids.push(tid);
        }
    }
    tids.sort_unstable();
    let task_count = tids.len();

    let leader_wchan = read_text_best_effort(&PathBuf::from(format!("/proc/{pid}/wchan")), 1024)
        .trim()
        .to_string();
    let leader_syscall =
        read_text_best_effort(&PathBuf::from(format!("/proc/{pid}/syscall")), 4096)
            .trim()
            .to_string();
    let leader_syscall_nr = parse_proc_syscall_line(&leader_syscall).map(|s| s.nr);

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for tid in tids.into_iter().take(max_tasks) {
        let comm = read_text_best_effort(&task_dir.join(format!("{tid}/comm")), 1024)
            .trim()
            .to_string();
        let wchan = read_text_best_effort(&task_dir.join(format!("{tid}/wchan")), 1024)
            .trim()
            .to_string();
        let syscall = read_text_best_effort(&task_dir.join(format!("{tid}/syscall")), 4096)
            .trim()
            .to_string();
        (tid, comm, wchan, syscall).hash(&mut hasher);
    }
    let digest = hasher.finish();

    Some(TaskSignature {
        task_count,
        digest,
        leader_wchan,
        leader_syscall_nr,
    })
}

fn pids_by_ppid(ppid: u32) -> Result<Vec<u32>> {
    let output = Command::new("ps")
        .args(["-o", "pid=", "--ppid", &ppid.to_string()])
        .output()
        .context("ps --ppid")?;
    if !output.status.success() {
        bail!(
            "ps --ppid failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        if let Ok(pid) = s.parse::<u32>() {
            pids.push(pid);
        }
    }
    Ok(pids)
}

fn read_parent_pid(pid: u32) -> Option<u32> {
    let stat_text = read_text_best_effort(&PathBuf::from(format!("/proc/{pid}/stat")), 64 * 1024);
    if stat_text.starts_with("(unavailable:") {
        return None;
    }
    parse_proc_stat_job_control(&stat_text).map(|jc| jc.ppid)
}

fn read_proc_comm(pid: u32) -> Option<String> {
    let p = PathBuf::from(format!("/proc/{pid}/comm"));
    let s = fs::read_to_string(p).ok()?;
    Some(s.trim().to_string())
}

fn read_proc_cmdline(pid: u32, max_bytes: usize) -> Option<String> {
    let p = PathBuf::from(format!("/proc/{pid}/cmdline"));
    let bytes = fs::read(p).ok()?;
    let clipped = if bytes.len() > max_bytes {
        &bytes[..max_bytes]
    } else {
        &bytes[..]
    };
    let mut s = String::new();
    for (i, part) in clipped
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .enumerate()
    {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&String::from_utf8_lossy(part));
    }
    if bytes.len() > max_bytes {
        s.push_str(" …(clipped)…");
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn find_vm_like_descendant_pid(root_pid: u32, max_depth: usize, max_nodes: usize) -> Option<u32> {
    use std::collections::VecDeque;
    let mut q: VecDeque<(u32, usize)> = VecDeque::new();
    q.push_back((root_pid, 0));
    let mut visited = 0usize;

    while let Some((pid, depth)) = q.pop_front() {
        visited += 1;
        if visited > max_nodes {
            break;
        }

        if let Some(comm) = read_proc_comm(pid) {
            if comm.starts_with("VM:") {
                return Some(pid);
            }
        }

        if depth >= max_depth {
            continue;
        }

        let Ok(children) = pids_by_ppid(pid) else {
            continue;
        };
        for c in children {
            q.push_back((c, depth + 1));
        }
    }

    None
}

fn read_job_control(pid: u32) -> Option<ProcStatJobControl> {
    let stat_text = read_text_best_effort(&PathBuf::from(format!("/proc/{pid}/stat")), 64 * 1024);
    if stat_text.starts_with("(unavailable:") {
        return None;
    }
    parse_proc_stat_job_control(&stat_text)
}

fn is_foreground_pgrp(jc: &ProcStatJobControl) -> Option<bool> {
    if jc.tty_nr == 0 || jc.tpgid <= 0 {
        return None;
    }
    Some(jc.pgrp == jc.tpgid)
}

fn append_job_control_compare(out: &mut String, pid: u32, parent_pid: Option<u32>) {
    let jc = read_job_control(pid);
    let pjc = parent_pid.and_then(read_job_control);
    let comm = read_proc_comm(pid).unwrap_or_else(|| "(unknown)".to_string());
    let pcomm = parent_pid
        .and_then(read_proc_comm)
        .unwrap_or_else(|| "(none)".to_string());

    out.push_str(&format!("pid={pid} comm={comm}\n"));
    if let Some(jc) = jc {
        out.push_str(&format!(
            "  tty_nr={}{} tpgid={} pgrp={} fg={}\n",
            jc.tty_nr,
            format_tty_nr_details(jc.tty_nr),
            jc.tpgid,
            jc.pgrp,
            match is_foreground_pgrp(&jc) {
                Some(true) => "yes",
                Some(false) => "no",
                None => "(n/a)",
            }
        ));
    } else {
        out.push_str("  (job control unavailable)\n");
    }

    if let Some(ppid) = parent_pid {
        out.push_str(&format!("parent_pid={ppid} comm={pcomm}\n"));
        if let Some(pjc) = pjc {
            out.push_str(&format!(
                "  tty_nr={}{} tpgid={} pgrp={} fg={}\n",
                pjc.tty_nr,
                format_tty_nr_details(pjc.tty_nr),
                pjc.tpgid,
                pjc.pgrp,
                match is_foreground_pgrp(&pjc) {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "(n/a)",
                }
            ));
        } else {
            out.push_str("  (job control unavailable)\n");
        }
    } else {
        out.push_str("parent_pid=(none)\n");
    }

    if let (Some(jc), Some(pjc)) = (jc, pjc) {
        if jc.tty_nr != 0 && jc.tty_nr == pjc.tty_nr {
            out.push_str(&format!(
                "tty_match=yes tty_foreground_pgrp={} target_is_fg={} parent_is_fg={}\n",
                jc.tpgid,
                match is_foreground_pgrp(&jc) {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "(n/a)",
                },
                match is_foreground_pgrp(&pjc) {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "(n/a)",
                },
            ));

            // Identify who owns the foreground TTY process group at the moment of snapshot.
            if jc.tpgid > 0 {
                let fg_pid = jc.tpgid as u32;
                let fg_comm = read_proc_comm(fg_pid).unwrap_or_else(|| "(unknown)".to_string());
                let fg_cmd =
                    read_proc_cmdline(fg_pid, 4096).unwrap_or_else(|| "(no cmdline)".to_string());
                out.push_str(&format!(
                    "tty_foreground_owner: pid={fg_pid} comm={fg_comm}\n"
                ));
                out.push_str(&format!("tty_foreground_owner_cmdline: {fg_cmd}\n"));
            }
        } else {
            out.push_str("tty_match=(unknown_or_no)\n");
        }
    }
}

fn snapshot_proc(out: &mut String, pid: u32, label: &str) {
    out.push_str(&format!("[{label}] /proc/{pid}/status\n"));
    append_proc_file(out, pid, "status", 64 * 1024);
    out.push_str("\n");

    out.push_str(&format!("[{label}] /proc/{pid}/maps (line count)\n"));
    let maps_path = PathBuf::from(format!("/proc/{pid}/maps"));
    match count_lines_streaming(&maps_path) {
        Ok(n) => out.push_str(&format!("maps_lines={n}\n")),
        Err(e) => out.push_str(&format!("(unavailable: {e})\n")),
    }
    out.push_str("\n");

    // Decode signal masks and job-control state from /proc, to make TTY stop causes
    // obvious without manual bitmask decoding.
    out.push_str(&format!("[{label}] status_signals_decoded\n"));
    let status_text =
        read_text_best_effort(&PathBuf::from(format!("/proc/{pid}/status")), 64 * 1024);
    if status_text.starts_with("(unavailable:") {
        out.push_str(&status_text);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    } else {
        append_decoded_status_signals(out, &status_text);
    }
    out.push_str("\n");

    out.push_str(&format!(
        "[{label}] job_control (from /proc/{pid}/stat + stdio)\n"
    ));
    let stat_text = read_text_best_effort(&PathBuf::from(format!("/proc/{pid}/stat")), 64 * 1024);
    if stat_text.starts_with("(unavailable:") {
        out.push_str(&stat_text);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    } else if let Some(jc) = parse_proc_stat_job_control(&stat_text) {
        let fg = if jc.tty_nr != 0 && jc.tpgid > 0 {
            if jc.pgrp == jc.tpgid {
                "yes"
            } else {
                "no"
            }
        } else {
            "(n/a)"
        };
        out.push_str(&format!(
            "state={} ppid={} pgrp={} session={} tpgid={} tty_nr={}{} is_foreground_pgrp={}\n",
            jc.state,
            jc.ppid,
            jc.pgrp,
            jc.session,
            jc.tpgid,
            jc.tty_nr,
            format_tty_nr_details(jc.tty_nr),
            fg,
        ));

        let fd0 = read_fd_target(pid, 0);
        let fd1 = read_fd_target(pid, 1);
        let fd2 = read_fd_target(pid, 2);
        out.push_str(&format!("fd0={fd0}\nfd1={fd1}\nfd2={fd2}\n"));
    } else {
        out.push_str("(unavailable: failed to parse /proc/<pid>/stat)\n");
    }
    out.push_str("\n");

    out.push_str(&format!("[{label}] /proc/{pid}/wchan\n"));
    append_proc_file(out, pid, "wchan", 8 * 1024);
    out.push_str("\n");

    out.push_str(&format!("[{label}] /proc/{pid}/stack\n"));
    append_proc_file(out, pid, "stack", 64 * 1024);
    out.push_str("\n");

    out.push_str(&format!("[{label}] /proc/{pid}/syscall\n"));
    append_proc_file(out, pid, "syscall", 8 * 1024);
    out.push_str("\n");

    out.push_str(&format!("[{label}] /proc/{pid}/task/* (sample)\n"));
    let task_discovered = snapshot_tasks(out, pid, 24);
    out.push_str("\n");

    if !task_discovered.ppoll_pipe_inodes.is_empty() {
        out.push_str(&format!(
            "[{label}] pipe_wakeup_path (from ppoll eventfd+pipe)\n"
        ));
        emit_pipe_wakeup_path(out, &task_discovered.ppoll_pipe_inodes, 4, 512, 256, 10);
        out.push_str("\n");
    }

    if !task_discovered.poll_fds.is_empty() {
        out.push_str(&format!("[{label}] /proc/{pid}/fdinfo (ppoll fds)\n"));
        for fd in task_discovered.poll_fds.iter().copied().take(16) {
            let p = PathBuf::from(format!("/proc/{pid}/fdinfo/{fd}"));
            let text = read_text_best_effort(&p, 8 * 1024);
            out.push_str(&format!("-- fdinfo {fd} --\n"));
            if let Some(flags) = parse_fdinfo_flags(&text) {
                let access = access_mode_from_open_flags(flags);
                out.push_str(&format!(
                    "flags_octal={flags:o} flags_hex=0x{flags:x} access={access}\n"
                ));
            }
            out.push_str(&text);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        out.push_str("\n");
    }

    out.push_str(&format!("[{label}] /proc/{pid}/fd (sample)\n"));
    snapshot_fds(
        out,
        pid,
        64,
        &task_discovered.socket_inodes,
        &task_discovered.pipe_inodes,
    );
    out.push_str("\n");
}

#[derive(Debug, Clone, Copy)]
struct ProcStatJobControl {
    state: char,
    ppid: u32,
    pgrp: i32,
    session: i32,
    tty_nr: i32,
    tpgid: i32,
}

fn parse_proc_stat_job_control(stat_text: &str) -> Option<ProcStatJobControl> {
    // /proc/<pid>/stat format: pid (comm) state ppid pgrp session tty_nr tpgid ...
    let s = stat_text.trim();
    let rparen = s.rfind(')')?;
    let after = s.get(rparen + 2..)?; // skip ") "
    let mut it = after.split_whitespace();
    let state_s = it.next()?;
    let state = state_s.chars().next()?;
    let ppid: u32 = it.next()?.parse().ok()?;
    let pgrp: i32 = it.next()?.parse().ok()?;
    let session: i32 = it.next()?.parse().ok()?;
    let tty_nr: i32 = it.next()?.parse().ok()?;
    let tpgid: i32 = it.next()?.parse().ok()?;
    Some(ProcStatJobControl {
        state,
        ppid,
        pgrp,
        session,
        tty_nr,
        tpgid,
    })
}

fn linux_major(dev: u32) -> u32 {
    (dev >> 8) & 0xfff
}

fn linux_minor(dev: u32) -> u32 {
    (dev & 0xff) | ((dev >> 12) & 0xfff00)
}

fn format_tty_nr_details(tty_nr: i32) -> String {
    if tty_nr == 0 {
        return " (no controlling tty)".to_string();
    }
    let dev = tty_nr as u32;
    let maj = linux_major(dev);
    let min = linux_minor(dev);
    format!(" dev=0x{dev:08x} major={maj} minor={min}")
}

fn append_decoded_status_signals(out: &mut String, status_text: &str) {
    let fields = ["SigPnd", "ShdPnd", "SigBlk", "SigIgn", "SigCgt"];
    let mut any = false;
    for key in fields {
        if let Some(mask) = parse_status_hex_mask(status_text, key) {
            let names = decode_signal_mask(mask);
            any = true;
            out.push_str(&format!("{key}: 0x{mask:x}\n"));
            if names.is_empty() {
                out.push_str("  (none)\n");
            } else {
                out.push_str(&format!("  {}\n", names.join(" ")));
            }
        }
    }
    if !any {
        out.push_str("(no signal masks found)\n");
    }
}

fn parse_status_hex_mask(status_text: &str, key: &str) -> Option<u128> {
    let prefix = format!("{key}:\t");
    for line in status_text.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            let hex = rest.trim();
            let hex = hex.strip_prefix("0x").unwrap_or(hex);
            return u128::from_str_radix(hex, 16).ok();
        }
    }
    None
}

fn decode_signal_mask(mask: u128) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for bit in 0..128u32 {
        if (mask & (1u128 << bit)) == 0 {
            continue;
        }
        let sig = bit + 1;
        out.push(signal_name(sig));
    }
    out
}

fn signal_name(sig: u32) -> String {
    match sig {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        3 => "SIGQUIT".into(),
        4 => "SIGILL".into(),
        5 => "SIGTRAP".into(),
        6 => "SIGABRT".into(),
        7 => "SIGBUS".into(),
        8 => "SIGFPE".into(),
        9 => "SIGKILL".into(),
        10 => "SIGUSR1".into(),
        11 => "SIGSEGV".into(),
        12 => "SIGUSR2".into(),
        13 => "SIGPIPE".into(),
        14 => "SIGALRM".into(),
        15 => "SIGTERM".into(),
        16 => "SIGSTKFLT".into(),
        17 => "SIGCHLD".into(),
        18 => "SIGCONT".into(),
        19 => "SIGSTOP".into(),
        20 => "SIGTSTP".into(),
        21 => "SIGTTIN".into(),
        22 => "SIGTTOU".into(),
        23 => "SIGURG".into(),
        24 => "SIGXCPU".into(),
        25 => "SIGXFSZ".into(),
        26 => "SIGVTALRM".into(),
        27 => "SIGPROF".into(),
        28 => "SIGWINCH".into(),
        29 => "SIGIO".into(),
        30 => "SIGPWR".into(),
        31 => "SIGSYS".into(),
        // Linux SIGRTMIN is typically 34; 32/33 are reserved by glibc/NPTL.
        32 => "SIGRTMIN-2".into(),
        33 => "SIGRTMIN-1".into(),
        34..=64 => format!("SIGRTMIN+{}", sig - 34),
        _ => format!("SIG{sig}"),
    }
}

#[derive(Default)]
struct TaskDiscoveredInodes {
    socket_inodes: Vec<u64>,
    pipe_inodes: Vec<u64>,
    ppoll_pipe_inodes: Vec<u64>,
    poll_fds: Vec<u32>,
}

fn snapshot_tasks(out: &mut String, pid: u32, max_tasks: usize) -> TaskDiscoveredInodes {
    let task_dir = PathBuf::from(format!("/proc/{pid}/task"));
    let entries = match fs::read_dir(&task_dir) {
        Ok(e) => e,
        Err(e) => {
            out.push_str(&format!("(unavailable: {e})\n"));
            return TaskDiscoveredInodes::default();
        }
    };

    let mut tids: Vec<u32> = Vec::new();
    for ent in entries.flatten() {
        let s = ent.file_name().to_string_lossy().to_string();
        if let Ok(tid) = s.parse::<u32>() {
            tids.push(tid);
        }
    }
    tids.sort_unstable();

    out.push_str(&format!("task_count: {}\n", tids.len()));
    out.push_str("task_sample:\n");
    let mut discovered = TaskDiscoveredInodes::default();
    for tid in tids.into_iter().take(max_tasks) {
        let comm = read_text_best_effort(&task_dir.join(format!("{tid}/comm")), 1024)
            .trim()
            .to_string();
        let wchan = read_text_best_effort(&task_dir.join(format!("{tid}/wchan")), 1024)
            .trim()
            .to_string();
        let syscall = read_text_best_effort(&task_dir.join(format!("{tid}/syscall")), 4096)
            .trim()
            .to_string();
        let stack = read_text_best_effort(&task_dir.join(format!("{tid}/stack")), 8 * 1024);
        let stack_top = stack.lines().take(2).collect::<Vec<_>>().join(" | ");
        out.push_str(&format!(
            "  tid {tid}: comm={comm} wchan={wchan} syscall={syscall} stack_top={stack_top}\n"
        ));

        if let Some(sc) = parse_proc_syscall_line(&syscall) {
            // On aarch64, syscall 73 is ppoll.
            if sc.nr == 73 {
                let pollfd_ptr = sc.args[0];
                let nfds = sc.args[1] as usize;
                if (1..=8).contains(&nfds) {
                    let mut pollfds: Vec<libc::pollfd> = vec![unsafe { std::mem::zeroed() }; nfds];
                    match read_remote_pollfds(pid, pollfd_ptr, &mut pollfds) {
                        Ok(()) => {
                            out.push_str(&format!(
                                "    ppoll decoded: nfds={nfds} pollfd_ptr=0x{pollfd_ptr:x}\n"
                            ));
                            let mut ppoll_has_eventfd = false;
                            let mut ppoll_pipe_inodes: Vec<u64> = Vec::new();
                            for (i, pfd) in pollfds.iter().enumerate() {
                                let fd = pfd.fd;
                                let target = if fd >= 0 {
                                    discovered.poll_fds.push(fd as u32);
                                    read_fd_target(pid, fd as u32)
                                } else {
                                    "(negative fd)".to_string()
                                };
                                if target.contains("anon_inode:[eventfd]") {
                                    ppoll_has_eventfd = true;
                                }
                                out.push_str(&format!(
									"      [{i}] fd={fd} events=0x{:04x} revents=0x{:04x} target={target}\n",
									(pfd.events as i16) as u16,
									(pfd.revents as i16) as u16,
								));
                                if let Some(inode) = parse_socket_inode(&target) {
                                    discovered.socket_inodes.push(inode);
                                }
                                if let Some(inode) = parse_pipe_inode(&target) {
                                    discovered.pipe_inodes.push(inode);
                                    ppoll_pipe_inodes.push(inode);
                                }
                            }
                            if ppoll_has_eventfd {
                                discovered.ppoll_pipe_inodes.extend(ppoll_pipe_inodes);
                            }
                        }
                        Err(e) => {
                            out.push_str(&format!(
								"    ppoll decoded: nfds={nfds} pollfd_ptr=0x{pollfd_ptr:x} (unavailable: {e})\n"
							));
                        }
                    }
                }
            }
        }
    }

    discovered.socket_inodes.sort_unstable();
    discovered.socket_inodes.dedup();
    discovered.pipe_inodes.sort_unstable();
    discovered.pipe_inodes.dedup();
    discovered.ppoll_pipe_inodes.sort_unstable();
    discovered.ppoll_pipe_inodes.dedup();
    discovered.poll_fds.sort_unstable();
    discovered.poll_fds.dedup();
    discovered
}

fn emit_pid_status_key_fields(out: &mut String, pid: u32) {
    let status = read_text_best_effort(&PathBuf::from(format!("/proc/{pid}/status")), 64 * 1024);
    out.push_str(&filter_lines(&status, |l| {
        l.starts_with("Name:")
            || l.starts_with("State:")
            || l.starts_with("PPid:")
            || l.starts_with("Threads:")
            || l.starts_with("VmRSS:")
            || l.starts_with("VmSize:")
            || l.starts_with("FDSize:")
    }));
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

fn emit_pipe_wakeup_path(
    out: &mut String,
    ppoll_pipe_inodes: &[u64],
    max_inodes: usize,
    max_pids: usize,
    max_fds_per_pid: usize,
    max_hits_per_inode: usize,
) {
    let mut inodes: Vec<u64> = ppoll_pipe_inodes.to_vec();
    inodes.sort_unstable();
    inodes.dedup();
    if inodes.is_empty() {
        out.push_str("(no ppoll pipe inodes)\n");
        return;
    }

    out.push_str("pipe_wakeup_path:\n");

    let proc_entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(e) => {
            out.push_str(&format!("(unavailable: {e})\n"));
            return;
        }
    };
    let mut proc_pids: Vec<u32> = Vec::new();
    for ent in proc_entries.flatten() {
        let s = ent.file_name().to_string_lossy().to_string();
        if let Ok(pid) = s.parse::<u32>() {
            proc_pids.push(pid);
        }
    }
    proc_pids.sort_unstable();

    for inode in inodes.into_iter().take(max_inodes) {
        out.push_str(&format!("-- pipe_inode {inode} (writer candidates) --\n"));
        let mut hit_counts: HashMap<u64, usize> = HashMap::new();
        hit_counts.insert(inode, 0);

        let mut scanned_pids = 0usize;
        let mut skipped_pids = 0usize;
        let mut proc_errs = 0usize;
        let mut writer_pids: Vec<u32> = Vec::new();

        for other_pid in proc_pids.iter().copied() {
            if scanned_pids >= max_pids {
                break;
            }
            scanned_pids += 1;
            let fd_dir = PathBuf::from(format!("/proc/{other_pid}/fd"));
            let fds = match fs::read_dir(&fd_dir) {
                Ok(e) => e,
                Err(_) => {
                    skipped_pids += 1;
                    continue;
                }
            };

            let mut comm: Option<String> = None;
            let mut scanned_fds = 0usize;
            for fd_ent in fds.flatten() {
                if scanned_fds >= max_fds_per_pid {
                    break;
                }
                scanned_fds += 1;
                let fd_name = fd_ent.file_name().to_string_lossy().to_string();
                let Ok(fd_num) = fd_name.parse::<u32>() else {
                    continue;
                };
                let target = match fs::read_link(fd_dir.join(fd_num.to_string())) {
                    Ok(t) => t.display().to_string(),
                    Err(_) => {
                        proc_errs += 1;
                        continue;
                    }
                };
                let Some(found_inode) = parse_pipe_inode(&target) else {
                    continue;
                };
                if found_inode != inode {
                    continue;
                }
                let count = hit_counts.entry(inode).or_insert(0);
                if *count >= max_hits_per_inode {
                    continue;
                }

                let fdinfo_path = PathBuf::from(format!("/proc/{other_pid}/fdinfo/{fd_num}"));
                let fdinfo = read_text_best_effort(&fdinfo_path, 8 * 1024);
                let mut access = "(unknown)";
                if let Some(flags) = parse_fdinfo_flags(&fdinfo) {
                    access = access_mode_from_open_flags(flags);
                    out.push_str(&format!(
						"  inode={inode} pid={other_pid} fd={fd_num} flags_octal={flags:o} flags_hex=0x{flags:x} access={access}\n"
					));
                } else {
                    out.push_str(&format!(
                        "  inode={inode} pid={other_pid} fd={fd_num} (no flags)\n"
                    ));
                }

                if access == "wronly" || access == "rdwr" {
                    let comm_s = comm.get_or_insert_with(|| {
                        read_text_best_effort(
                            &PathBuf::from(format!("/proc/{other_pid}/comm")),
                            1024,
                        )
                        .trim()
                        .to_string()
                    });
                    out.push_str(&format!("    comm={comm_s}\n"));
                    writer_pids.push(other_pid);
                }

                *count += 1;
            }
        }

        writer_pids.sort_unstable();
        writer_pids.dedup();
        if writer_pids.is_empty() {
            out.push_str("  (no writer owners found within scan bounds)\n");
        } else {
            out.push_str("  writer_pid_task_samples:\n");
            for wp in writer_pids.into_iter().take(6) {
                out.push_str(&format!("  --- writer_pid {wp} ---\n"));
                emit_pid_status_key_fields(out, wp);
                let _ = snapshot_tasks(out, wp, 12);
                // One-hop recursion: if the writer PID is itself waiting on an eventfd+pipe
                // ppoll set, follow that pipe inode to its writer owners.
                let next_pipe_inodes = collect_ppoll_eventfd_pipe_inodes(wp, 24);
                if !next_pipe_inodes.is_empty() {
                    out.push_str("  writer_wait_graph_one_hop:\n");
                    emit_one_hop_pipe_wait_graph(
                        out,
                        wp,
                        &next_pipe_inodes,
                        max_pids,
                        max_fds_per_pid,
                        max_hits_per_inode,
                    );
                }
            }
        }

        out.push_str(&format!(
			"  wakeup_path_stats: scanned_pids={scanned_pids} skipped_pids={skipped_pids} fd_read_errors={proc_errs}\n"
		));
    }
}

fn emit_one_hop_pipe_wait_graph(
    out: &mut String,
    pid: u32,
    pipe_inodes: &[u64],
    max_pids: usize,
    max_fds_per_pid: usize,
    max_hits_per_inode: usize,
) {
    let mut inodes: Vec<u64> = pipe_inodes.to_vec();
    inodes.sort_unstable();
    inodes.dedup();
    out.push_str(&format!(
        "    pid={pid} waits_on_eventfd_pipe_inodes: {inodes:?}\n"
    ));
    for inode in inodes.into_iter().take(3) {
        out.push_str(&format!("    -- waits_on pipe_inode {inode} --\n"));
        let writer_pids =
            collect_pipe_writer_pids(&[inode], max_pids, max_fds_per_pid, max_hits_per_inode);
        if writer_pids.is_empty() {
            out.push_str("      (no writer owners found within scan bounds)\n");
            continue;
        }
        out.push_str(&format!("      writer_pids: {writer_pids:?}\n"));
        for wp in writer_pids.into_iter().take(4) {
            out.push_str(&format!("      --- owner_pid {wp} ---\n"));
            emit_pid_status_key_fields(out, wp);
            if let Some(sig) = sample_task_signature(wp, 8) {
                out.push_str(&format!(
					"      signature: tasks={} leader_wchan={} leader_syscall_nr={} digest=0x{:x}\n",
					sig.task_count,
					sig.leader_wchan,
					sig.leader_syscall_nr
						.map(|n| n.to_string())
						.unwrap_or_else(|| "?".to_string()),
					sig.digest
				));
            }
        }
    }
}

fn snapshot_fds(
    out: &mut String,
    pid: u32,
    max_fds: usize,
    extra_socket_inodes: &[u64],
    extra_pipe_inodes: &[u64],
) {
    let fd_dir = PathBuf::from(format!("/proc/{pid}/fd"));
    let entries = match fs::read_dir(&fd_dir) {
        Ok(e) => e,
        Err(e) => {
            out.push_str(&format!("(unavailable: {e})\n"));
            return;
        }
    };

    let mut fds: Vec<u32> = Vec::new();
    for ent in entries.flatten() {
        let name = ent.file_name();
        let s = name.to_string_lossy();
        if let Ok(n) = s.parse::<u32>() {
            fds.push(n);
        }
    }
    fds.sort_unstable();

    let mut targets_by_fd: HashMap<u32, String> = HashMap::new();
    for fd in &fds {
        let link = fd_dir.join(fd.to_string());
        let target = match fs::read_link(&link) {
            Ok(t) => t.display().to_string(),
            Err(e) => format!("(unreadable: {e})"),
        };
        targets_by_fd.insert(*fd, target);
    }

    out.push_str(&format!("fd_count: {}\n", fds.len()));
    out.push_str("fd_targets:\n");
    for fd in fds.iter().copied().take(max_fds) {
        let target = targets_by_fd
            .get(&fd)
            .cloned()
            .unwrap_or_else(|| "(unknown)".to_string());
        out.push_str(&format!("  fd {fd}: {target}\n"));
    }
    if fds.len() > max_fds {
        out.push_str(&format!("  … ({} more fds) …\n", fds.len() - max_fds));
    }

    let epoll_fds: Vec<u32> = fds
        .iter()
        .copied()
        .filter(|fd| {
            targets_by_fd
                .get(fd)
                .map(|t| t.contains("anon_inode:[eventpoll]"))
                .unwrap_or(false)
        })
        .collect();

    out.push_str("epoll_fdinfo:\n");
    let mut observed_tfds: HashSet<u32> = HashSet::new();
    for fd in epoll_fds.iter().copied().take(16) {
        let p = PathBuf::from(format!("/proc/{pid}/fdinfo/{fd}"));
        let text = read_text_best_effort(&p, 64 * 1024);
        out.push_str(&format!("-- epoll fdinfo {fd} --\n"));
        out.push_str(&text);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        for line in text.lines() {
            let l = line.trim_start();
            if let Some(rest) = l.strip_prefix("tfd:") {
                let num = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u32>().ok());
                if let Some(n) = num {
                    observed_tfds.insert(n);
                }
            }
        }
    }

    let mut socket_inodes: Vec<u64> = extra_socket_inodes.to_vec();
    let mut pipe_inodes: Vec<u64> = extra_pipe_inodes.to_vec();

    if !observed_tfds.is_empty() {
        let mut tfds: Vec<u32> = observed_tfds.into_iter().collect();
        tfds.sort_unstable();
        out.push_str("epoll_tfd_targets:\n");
        for tfd in tfds.into_iter().take(64) {
            let target = targets_by_fd
                .get(&tfd)
                .cloned()
                .unwrap_or_else(|| "(unknown)".to_string());
            out.push_str(&format!("  tfd {tfd}: {target}\n"));
            if let Some(inode) = parse_socket_inode(&target) {
                socket_inodes.push(inode);
            }
            if let Some(inode) = parse_pipe_inode(&target) {
                pipe_inodes.push(inode);
            }
        }
    }

    pipe_inodes.sort_unstable();
    pipe_inodes.dedup();
    if !pipe_inodes.is_empty() {
        emit_pipe_inode_fd_owners(out, &pipe_inodes, 512, 256, 10);
    }

    // Resolve any observed socket:[inode] entries via /proc/net/*.
    socket_inodes.sort_unstable();
    socket_inodes.dedup();
    if !socket_inodes.is_empty() {
        out.push_str("unix_socket_inode_lookup:\n");
        let unix = fs::read_to_string("/proc/net/unix")
            .unwrap_or_else(|e| format!("(unavailable: {e})\n"));
        let tcp =
            fs::read_to_string("/proc/net/tcp").unwrap_or_else(|e| format!("(unavailable: {e})\n"));
        let tcp6 = fs::read_to_string("/proc/net/tcp6")
            .unwrap_or_else(|e| format!("(unavailable: {e})\n"));
        let udp =
            fs::read_to_string("/proc/net/udp").unwrap_or_else(|e| format!("(unavailable: {e})\n"));
        let udp6 = fs::read_to_string("/proc/net/udp6")
            .unwrap_or_else(|e| format!("(unavailable: {e})\n"));
        let raw =
            fs::read_to_string("/proc/net/raw").unwrap_or_else(|e| format!("(unavailable: {e})\n"));
        let raw6 = fs::read_to_string("/proc/net/raw6")
            .unwrap_or_else(|e| format!("(unavailable: {e})\n"));
        let netlink = fs::read_to_string("/proc/net/netlink")
            .unwrap_or_else(|e| format!("(unavailable: {e})\n"));

        for inode in socket_inodes.iter().copied().take(64) {
            out.push_str(&format!("-- inode {inode} --\n"));
            emit_proc_net_inode_matches(out, "/proc/net/unix", &unix, inode);
            emit_proc_net_inode_matches(out, "/proc/net/tcp", &tcp, inode);
            emit_proc_net_inode_matches(out, "/proc/net/tcp6", &tcp6, inode);
            emit_proc_net_inode_matches(out, "/proc/net/udp", &udp, inode);
            emit_proc_net_inode_matches(out, "/proc/net/udp6", &udp6, inode);
            emit_proc_net_inode_matches(out, "/proc/net/raw", &raw, inode);
            emit_proc_net_inode_matches(out, "/proc/net/raw6", &raw6, inode);
            emit_proc_net_inode_matches(out, "/proc/net/netlink", &netlink, inode);
        }

        // Best-effort: resolve which processes own these socket inodes by scanning /proc/*/fd.
        // This stays "all Rust" (no external tooling) and is bounded for performance.
        emit_socket_inode_fd_owners(out, &socket_inodes, 512, 256, 10);
    }

    out.push_str("fdinfo_sample:\n");
    for fd in fds.iter().copied().take(12) {
        let p = PathBuf::from(format!("/proc/{pid}/fdinfo/{fd}"));
        out.push_str(&format!("-- fdinfo {fd} --\n"));
        out.push_str(&read_text_best_effort(&p, 8 * 1024));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
}

fn emit_socket_inode_fd_owners(
    out: &mut String,
    inodes: &[u64],
    max_pids: usize,
    max_fds_per_pid: usize,
    max_hits_per_inode: usize,
) {
    let wanted: HashSet<u64> = inodes.iter().copied().collect();
    if wanted.is_empty() {
        return;
    }

    out.push_str("socket_inode_fd_owners:\n");

    let proc_entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(e) => {
            out.push_str(&format!("(unavailable: {e})\n"));
            return;
        }
    };

    // Keep per-inode hit counts so we can stop early.
    let mut hit_counts: HashMap<u64, usize> = HashMap::new();
    for inode in inodes {
        hit_counts.insert(*inode, 0);
    }

    let mut scanned_pids = 0usize;
    let mut skipped_pids = 0usize;
    let mut proc_errs = 0usize;

    for ent in proc_entries.flatten() {
        if scanned_pids >= max_pids {
            break;
        }
        let name = ent.file_name();
        let s = name.to_string_lossy();
        let Ok(other_pid) = s.parse::<u32>() else {
            continue;
        };

        // If we've already satisfied all inodes, stop early.
        if hit_counts.values().all(|c| *c >= max_hits_per_inode) {
            break;
        }

        scanned_pids += 1;
        let fd_dir = PathBuf::from(format!("/proc/{other_pid}/fd"));
        let fds = match fs::read_dir(&fd_dir) {
            Ok(e) => e,
            Err(_) => {
                skipped_pids += 1;
                continue;
            }
        };

        // Lazily read comm only if we find a hit.
        let mut comm: Option<String> = None;
        let mut scanned_fds = 0usize;
        for fd_ent in fds.flatten() {
            if scanned_fds >= max_fds_per_pid {
                break;
            }
            scanned_fds += 1;
            let fd_name = fd_ent.file_name().to_string_lossy().to_string();
            let Ok(fd_num) = fd_name.parse::<u32>() else {
                continue;
            };
            let target = match fs::read_link(fd_dir.join(fd_num.to_string())) {
                Ok(t) => t.display().to_string(),
                Err(_) => {
                    proc_errs += 1;
                    continue;
                }
            };
            let Some(inode) = parse_socket_inode(&target) else {
                continue;
            };
            if !wanted.contains(&inode) {
                continue;
            }
            let count = hit_counts.entry(inode).or_insert(0);
            if *count >= max_hits_per_inode {
                continue;
            }

            let comm_s = comm.get_or_insert_with(|| {
                read_text_best_effort(&PathBuf::from(format!("/proc/{other_pid}/comm")), 1024)
                    .trim()
                    .to_string()
            });
            out.push_str(&format!(
                "  inode={inode} pid={other_pid} comm={comm_s} fd={fd_num}\n"
            ));
            *count += 1;
        }
    }

    out.push_str(&format!(
		"socket_inode_fd_owners_stats: scanned_pids={scanned_pids} skipped_pids={skipped_pids} fd_read_errors={proc_errs}\n"
	));
}

fn parse_socket_inode(target: &str) -> Option<u64> {
    // Targets look like: "socket:[3073]".
    let s = target.strip_prefix("socket:[")?;
    let s = s.strip_suffix(']')?;
    s.parse::<u64>().ok()
}

fn parse_pipe_inode(target: &str) -> Option<u64> {
    // Targets look like: "pipe:[3073]".
    let s = target.strip_prefix("pipe:[")?;
    let s = s.strip_suffix(']')?;
    s.parse::<u64>().ok()
}

fn emit_pipe_inode_fd_owners(
    out: &mut String,
    inodes: &[u64],
    max_pids: usize,
    max_fds_per_pid: usize,
    max_hits_per_inode: usize,
) {
    let wanted: HashSet<u64> = inodes.iter().copied().collect();
    if wanted.is_empty() {
        return;
    }

    out.push_str("pipe_inode_fd_owners:\n");

    let proc_entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(e) => {
            out.push_str(&format!("(unavailable: {e})\n"));
            return;
        }
    };

    let mut hit_counts: HashMap<u64, usize> = HashMap::new();
    for inode in inodes {
        hit_counts.insert(*inode, 0);
    }

    let mut scanned_pids = 0usize;
    let mut skipped_pids = 0usize;
    let mut proc_errs = 0usize;

    for ent in proc_entries.flatten() {
        if scanned_pids >= max_pids {
            break;
        }
        let name = ent.file_name();
        let s = name.to_string_lossy();
        let Ok(other_pid) = s.parse::<u32>() else {
            continue;
        };

        if hit_counts.values().all(|c| *c >= max_hits_per_inode) {
            break;
        }

        scanned_pids += 1;
        let fd_dir = PathBuf::from(format!("/proc/{other_pid}/fd"));
        let fds = match fs::read_dir(&fd_dir) {
            Ok(e) => e,
            Err(_) => {
                skipped_pids += 1;
                continue;
            }
        };

        let mut comm: Option<String> = None;
        let mut scanned_fds = 0usize;
        for fd_ent in fds.flatten() {
            if scanned_fds >= max_fds_per_pid {
                break;
            }
            scanned_fds += 1;
            let fd_name = fd_ent.file_name().to_string_lossy().to_string();
            let Ok(fd_num) = fd_name.parse::<u32>() else {
                continue;
            };
            let target = match fs::read_link(fd_dir.join(fd_num.to_string())) {
                Ok(t) => t.display().to_string(),
                Err(_) => {
                    proc_errs += 1;
                    continue;
                }
            };
            let Some(inode) = parse_pipe_inode(&target) else {
                continue;
            };
            if !wanted.contains(&inode) {
                continue;
            }
            let count = hit_counts.entry(inode).or_insert(0);
            if *count >= max_hits_per_inode {
                continue;
            }

            let comm_s = comm.get_or_insert_with(|| {
                read_text_best_effort(&PathBuf::from(format!("/proc/{other_pid}/comm")), 1024)
                    .trim()
                    .to_string()
            });
            out.push_str(&format!(
                "  inode={inode} pid={other_pid} comm={comm_s} fd={fd_num}\n"
            ));
            let fdinfo_path = PathBuf::from(format!("/proc/{other_pid}/fdinfo/{fd_num}"));
            let fdinfo = read_text_best_effort(&fdinfo_path, 8 * 1024);
            if let Some(flags) = parse_fdinfo_flags(&fdinfo) {
                let access = access_mode_from_open_flags(flags);
                out.push_str(&format!(
                    "    flags_octal={flags:o} flags_hex=0x{flags:x} access={access}\n"
                ));
            }
            out.push_str("    fdinfo:\n");
            for line in fdinfo.lines().take(32) {
                out.push_str("      ");
                out.push_str(line);
                out.push('\n');
            }
            *count += 1;
        }
    }

    out.push_str(&format!(
		"pipe_inode_fd_owners_stats: scanned_pids={scanned_pids} skipped_pids={skipped_pids} fd_read_errors={proc_errs}\n"
	));
}

#[derive(Clone, Copy, Debug)]
struct ProcSyscall {
    nr: u64,
    args: [u64; 6],
}

fn parse_proc_syscall_line(line: &str) -> Option<ProcSyscall> {
    let mut it = line.split_whitespace();
    let nr = parse_u64_mixed(it.next()?)?;
    let mut args = [0u64; 6];
    for i in 0..6 {
        args[i] = parse_u64_mixed(it.next()?)?;
    }
    Some(ProcSyscall { nr, args })
}

fn parse_u64_mixed(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

fn parse_fdinfo_flags(fdinfo: &str) -> Option<u64> {
    for line in fdinfo.lines() {
        let l = line.trim_start();
        let Some(rest) = l.strip_prefix("flags:") else {
            continue;
        };
        let tok = rest.split_whitespace().next()?;
        return u64::from_str_radix(tok.trim(), 8).ok();
    }
    None
}

fn access_mode_from_open_flags(flags: u64) -> &'static str {
    let accmode = flags & (libc::O_ACCMODE as u64);
    if accmode == (libc::O_WRONLY as u64) {
        "wronly"
    } else if accmode == (libc::O_RDWR as u64) {
        "rdwr"
    } else {
        // O_RDONLY is defined as 0.
        "rdonly"
    }
}

fn read_fd_target(pid: u32, fd: u32) -> String {
    let link = PathBuf::from(format!("/proc/{pid}/fd/{fd}"));
    match fs::read_link(&link) {
        Ok(t) => t.display().to_string(),
        Err(e) => format!("(unreadable: {e})"),
    }
}

fn read_remote_pollfds(
    pid: u32,
    pollfd_ptr: u64,
    pollfds: &mut [libc::pollfd],
) -> std::result::Result<(), String> {
    if pollfds.is_empty() {
        return Ok(());
    }
    let len = pollfds.len() * std::mem::size_of::<libc::pollfd>();
    let local_iov = libc::iovec {
        iov_base: pollfds.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: len,
    };
    let remote_iov = libc::iovec {
        iov_base: pollfd_ptr as usize as *mut libc::c_void,
        iov_len: len,
    };

    // Safety: process_vm_readv writes into our local buffer; remote pointer comes from the
    // target process syscall arguments and may be invalid. Errors are handled.
    let n = unsafe {
        libc::process_vm_readv(
            pid as libc::pid_t,
            &local_iov as *const libc::iovec,
            1,
            &remote_iov as *const libc::iovec,
            1,
            0,
        )
    };
    if n < 0 {
        let e = io::Error::last_os_error();
        return Err(e.to_string());
    }
    let n = n as usize;
    if n != len {
        return Err(format!("short read: {n} bytes (expected {len})"));
    }
    Ok(())
}

fn emit_proc_net_inode_matches(out: &mut String, table_name: &str, table_text: &str, inode: u64) {
    out.push_str(&format!("{table_name}:\n"));
    if table_text.starts_with("(unavailable:") {
        out.push_str(table_text);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        return;
    }

    let needle = inode.to_string();
    let mut matches = 0usize;
    for line in table_text.lines() {
        if line.split_whitespace().any(|tok| tok == needle) {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
            matches += 1;
            if matches >= 10 {
                out.push_str("  …(more matches)…\n");
                break;
            }
        }
    }
    if matches == 0 {
        out.push_str("  (no matches)\n");
    }
}

fn append_proc_file(out: &mut String, pid: u32, name: &str, max_bytes: usize) {
    let path = PathBuf::from(format!("/proc/{pid}/{name}"));
    match fs::read(&path) {
        Ok(bytes) => {
            let clipped = if bytes.len() > max_bytes {
                &bytes[..max_bytes]
            } else {
                &bytes[..]
            };
            out.push_str(&String::from_utf8_lossy(clipped));
            if bytes.len() > max_bytes {
                out.push_str("\n…(clipped)…\n");
            }
        }
        Err(e) => {
            out.push_str(&format!("(unavailable: {e})\n"));
        }
    }
}

fn write_ps(path: &Path, pid: u32) -> Result<()> {
    let mut out = String::new();
    out.push_str("### ps -o pid,ppid,etime,cmd (edge pid)\n");
    let ps_one = Command::new("ps")
        .args(["-o", "pid,ppid,etime,cmd", "-p", &pid.to_string()])
        .output();
    if let Ok(ps_one) = ps_one {
        out.push_str(&String::from_utf8_lossy(&ps_one.stdout));
        out.push_str(&String::from_utf8_lossy(&ps_one.stderr));
    }
    out.push_str("\n### ps -ef (edge-related, first 50)\n");
    let ps_all = Command::new("ps").arg("-ef").output();
    if let Ok(ps_all) = ps_all {
        let text = String::from_utf8_lossy(&ps_all.stdout);
        let mut lines = 0;
        for line in text.lines() {
            if line.contains("microsoft-edge")
                || line.contains("msedge")
                || line.contains("chrome")
                || line.contains("crashpad")
                || line.contains("FEXInterpreter")
            {
                out.push_str(line);
                out.push('\n');
                lines += 1;
                if lines >= 50 {
                    break;
                }
            }
        }
    }
    fs::write(path, out).context("write ps")
}

fn write_threads(path: &Path, pid: u32) -> Result<()> {
    let mut out = String::new();
    out.push_str("### thread_count_total\n");
    let total = Command::new("ps").args(["-eT"]).output();
    if let Ok(total) = total {
        out.push_str(&format!(
            "{}\n",
            String::from_utf8_lossy(&total.stdout).lines().count()
        ));
    } else {
        out.push_str("(unknown)\n");
    }
    out.push_str("### thread_count_edge\n");
    let edge = Command::new("ps")
        .args(["-T", "-p", &pid.to_string()])
        .output();
    if let Ok(edge) = edge {
        out.push_str(&format!(
            "{}\n",
            String::from_utf8_lossy(&edge.stdout).lines().count()
        ));
    } else {
        out.push_str("(unknown)\n");
    }
    fs::write(path, out).context("write threads")
}

fn targs_push_path(args: &mut Vec<String>, p: &Path) {
    args.push(p.display().to_string());
}

fn filter_stderr(input: &Path, output: &Path) -> Result<()> {
    let s = fs::read_to_string(input).context("read stderr")?;
    let filtered: String = s
        .lines()
        .filter(|l| !l.contains("crashpad") && !l.contains("ptrace:"))
        .map(|l| {
            let mut l = l.to_string();
            l.push('\n');
            l
        })
        .collect();
    fs::write(output, filtered).context("write filtered stderr")?;
    Ok(())
}

fn count_lines(path: &Path) -> Result<u64> {
    let content = fs::read(path).context("read file for line count")?;
    let mut lines = 0u64;
    for b in content {
        if b == b'\n' {
            lines += 1;
        }
    }
    Ok(lines)
}

fn count_lines_streaming(path: &Path) -> Result<u64> {
    use std::io::Read;

    let mut f = fs::File::open(path).context("open file for line count")?;
    let mut buf = [0u8; 64 * 1024];
    let mut lines = 0u64;
    loop {
        let n = f.read(&mut buf).context("read file for line count")?;
        if n == 0 {
            break;
        }
        for b in &buf[..n] {
            if *b == b'\n' {
                lines += 1;
            }
        }
    }
    Ok(lines)
}

fn count_substring_lines(path: &Path, needle: &str) -> Result<u64> {
    let s = fs::read_to_string(path).context("read file for substring count")?;
    Ok(s.lines().filter(|l| l.contains(needle)).count() as u64)
}

fn run_command_with_pty_to_file(
    args: &[String],
    log_path: &Path,
    timeout: Duration,
) -> Result<i32> {
    let res = run_command_with_pty_to_file_observed(args, log_path, timeout, None, &|_| {})?;
    Ok(res.exit_code)
}

fn ptsname(master: RawFd) -> Result<CString> {
    let mut buf = [0 as libc::c_char; 256];
    let rc = unsafe { libc::ptsname_r(master, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        bail!("ptsname_r failed: {}", io::Error::from_raw_os_error(rc));
    }

    let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Ok(CString::new(cstr.to_bytes())?)
}

fn set_nonblocking(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        bail!("fcntl(F_GETFL) failed: {}", io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        bail!("fcntl(F_SETFL) failed: {}", io::Error::last_os_error());
    }
    Ok(())
}

fn drain_master(master: RawFd, out: &mut fs::File) -> Result<()> {
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            out.write_all(&buf[..n as usize])?;
            continue;
        }
        if n == 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            break;
        }
        if err.raw_os_error() == Some(libc::EIO) {
            break;
        }
        break;
    }
    Ok(())
}

fn waitpid_nonblocking(pid: libc::pid_t) -> Result<Option<i32>> {
    let mut status: libc::c_int = 0;
    let rc = unsafe { libc::waitpid(pid, &mut status as *mut _, libc::WNOHANG) };
    if rc < 0 {
        return Err(anyhow::anyhow!(io::Error::last_os_error()));
    }
    if rc == 0 {
        return Ok(None);
    }
    Ok(Some(exit_status_code(status)))
}

fn waitpid_blocking(pid: libc::pid_t) -> Result<i32> {
    let mut status: libc::c_int = 0;
    let rc = unsafe { libc::waitpid(pid, &mut status as *mut _, 0) };
    if rc < 0 {
        return Err(anyhow::anyhow!(io::Error::last_os_error()));
    }
    Ok(exit_status_code(status))
}

fn exit_status_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

fn kill_process_group(pid: libc::pid_t, signal: libc::c_int) {
    unsafe {
        // Negative PID means process group.
        libc::kill(-pid, signal);
    }
}

fn kill_process_tree(root: u32, signal: libc::c_int, max_pids: usize) {
    let mut queue: Vec<u32> = vec![root];
    let mut seen: HashSet<u32> = HashSet::new();
    let mut all: Vec<u32> = Vec::new();

    while let Some(pid) = queue.pop() {
        if all.len() >= max_pids {
            break;
        }
        if !seen.insert(pid) {
            continue;
        }
        all.push(pid);
        if let Ok(children) = pids_by_ppid(pid) {
            for c in children {
                if !seen.contains(&c) {
                    queue.push(c);
                }
            }
        }
    }

    for pid in all.into_iter().rev() {
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
}

#[cfg(unix)]
unsafe fn child_fail(master: RawFd, step: &str, err: io::Error) -> ! {
    // Best-effort: write an error message to the PTY master so the parent captures it.
    // Avoid allocation-heavy formatting; this is a last-ditch path.
    let msg = format!("[edge-muvm-experiment] child failure at {step}: {err}\n");
    let _ = libc::write(
        master,
        msg.as_ptr() as *const libc::c_void,
        msg.as_bytes().len(),
    );
    libc::_exit(127);
}

fn chrono_stamp() -> String {
    // Avoid adding chrono dependency for a single stamp.
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_millis();
    format!("{ts}")
}

fn iso_now() -> String {
    // Minimal ISO-ish timestamp (seconds resolution).
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs();
    format!("unix-seconds:{ts}")
}

fn resolve_in_path(program: &str) -> Result<PathBuf> {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 {
        return Ok(candidate.to_path_buf());
    }

    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let full = dir.join(program);
        if full.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if fs::metadata(&full)
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
                {
                    return Ok(full);
                }
            }

            #[cfg(not(unix))]
            {
                return Ok(full);
            }
        }
    }

    bail!("{program} not found in PATH")
}
