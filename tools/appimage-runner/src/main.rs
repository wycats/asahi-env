use anyhow::{Context, Result};
use clap::Parser;
use clap::builder::BoolishValueParser;
use clap::{Args, Subcommand};
use serde::Serialize;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

#[cfg(feature = "squashfs-ng")]
use std::collections::HashMap;

#[derive(Parser)]
#[command(author, version, about, long_about = None, subcommand_precedence_over_arg = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[command(flatten)]
    legacy: LegacyRunArgs,
}

#[derive(Subcommand)]
enum Commands {
    /// Run an AppImage under muvm + FEX (evidence-first)
    Run(RunArgs),

    /// Run probes inside the guest (evidence-first)
    Probe(ProbeArgs),

    /// Internal: host-side PC/SC bridge (vsock -> pcscd unix socket)
    #[command(hide = true)]
    PcscHost(PcscHostArgs),

    /// Internal: guest-side PC/SC bridge (unix socket -> vsock)
    #[command(hide = true)]
    PcscGuest(PcscGuestArgs),
}

#[derive(Args, Clone, Debug)]
struct CommonGuestOpts {
    /// Environment variables to pass to the guest (KEY=VALUE)
    #[arg(short, long)]
    env: Vec<String>,

    /// FEX rootfs overlay image(s)
    #[arg(long)]
    fex_image: Vec<PathBuf>,

    /// Choose a default FEX image set when `--fex-image` isn't provided.
    ///
    /// - `auto`: prefer `fedora-base-x86_64.erofs` in the current directory if present,
    ///   otherwise fall back to common `sniper*.erofs` names.
    /// - `fedora`: require `fedora-base-x86_64.erofs` in the current directory.
    /// - `sniper`: prefer `sniper-sdk.erofs`, then `sniper.erofs`, then `sniper-debug.erofs`.
    #[arg(long, default_value = "auto", value_enum)]
    fex_profile: FexProfile,

    /// Path to the muvm binary to execute.
    #[arg(long, default_value = "muvm")]
    muvm_path: PathBuf,

    /// Additional arguments to pass through to muvm (before `--`).
    ///
    /// Note: muvm is order-sensitive for some flags (e.g. `--gpu-mode=drm`), which must appear
    /// before `--emu=fex`.
    #[arg(long, value_name = "ARG", allow_hyphen_values = true)]
    muvm_arg: Vec<OsString>,

    /// Optional capture guard: if set, terminate muvm after N seconds.
    /// This is intended for evidence collection when GUI apps block or await user input.
    #[arg(long)]
    timeout_seconds: Option<u64>,

    /// Optional shell snippet to run inside the guest before launching the AppImage.
    /// This runs under `/bin/bash -lc`.
    ///
    /// Example (force Firefox for xdg-open):
    ///   --guest-pre 'xdg-settings set default-web-browser org.mozilla.firefox.desktop || true'
    #[arg(long)]
    guest_pre: Option<String>,

    /// Enable a best-effort PC/SC bridge so x86_64 apps can talk to host pcscd without USB passthrough.
    ///
    /// This sets `PCSCLITE_CSOCK_NAME` inside the guest and spawns a guest-side unix socket proxy
    /// which forwards to a host-side vsock listener.
    #[arg(long, default_value_t = false)]
    pcsc_bridge: bool,

    /// Host vsock port to use for the PC/SC bridge.
    #[arg(long, default_value_t = 50050)]
    pcsc_vsock_port: u32,

    /// Path to the host pcscd unix socket.
    #[arg(long, default_value = "/run/pcscd/pcscd.comm")]
    pcsc_host_socket: PathBuf,

    /// Path to the guest pcsc-lite socket to create when `--pcsc-bridge` is enabled.
    ///
    /// We default to a user-writable location so this works without `--privileged`.
    #[arg(long, default_value = "/tmp/pcscd.comm")]
    pcsc_guest_socket: PathBuf,
}

#[derive(Args, Clone, Debug)]
struct PcscHostArgs {
    /// Vsock port to listen on
    #[arg(long, default_value_t = 50050)]
    port: u32,

    /// Host pcscd unix socket to connect to
    #[arg(long, default_value = "/run/pcscd/pcscd.comm")]
    pcsc_socket: PathBuf,
}

#[derive(Args, Clone, Debug)]
struct PcscGuestArgs {
    /// Vsock port to connect to on the host
    #[arg(long, default_value_t = 50050)]
    host_port: u32,

    /// Path for the guest unix socket to create for pcsc-lite clients
    #[arg(long, default_value = "/tmp/pcscd.comm")]
    listen: PathBuf,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum FexProfile {
    Auto,
    Fedora,
    Sniper,
}

#[derive(Args, Clone, Debug)]
struct ExtractionOpts {
    /// Strip the ELF .note.gnu.property section from x86_64 ELFs inside the extracted AppImage.
    /// Some AppImages advertise x86-64-v3/v4 (or CET) via this note, which FEX can reject.
    #[arg(
        long,
        default_value = "true",
        num_args = 1,
        action = clap::ArgAction::Set,
        value_parser = BoolishValueParser::new()
    )]
    strip_gnu_property: bool,

    /// Path to `objcopy` for stripping `.note.gnu.property`.
    ///
    /// If not provided, the runner will try `objcopy`, then `llvm-objcopy`, then `eu-objcopy`.
    /// Only used when `--strip-gnu-property=true`.
    #[arg(long)]
    objcopy_path: Option<PathBuf>,

    /// How to extract the embedded SquashFS filesystem.
    ///
    /// - `auto` (default): use `squashfs-ng` if compiled in, otherwise `unsquashfs`.
    /// - `unsquashfs`: spawn the external `unsquashfs` binary.
    /// - `squashfs-ng`: extract using the `squashfs-ng` Rust crate (requires the Cargo feature).
    #[arg(long, default_value = "auto", value_enum)]
    extract_with: ExtractWith,
}

#[derive(Args, Clone, Debug)]
struct RunArgs {
    /// Path to the AppImage file
    appimage: PathBuf,

    #[command(flatten)]
    guest: CommonGuestOpts,

    #[command(flatten)]
    extraction: ExtractionOpts,

    /// Output directory for evidence artifacts.
    ///
    /// If not provided, defaults to `docs/agent-context/research/<app>/<timestamp>/`.
    #[arg(long)]
    out_dir: Option<PathBuf>,

    /// Optional: also write a JSON report to this path (legacy compatibility).
    #[arg(long)]
    report: Option<PathBuf>,

    /// Arguments to pass to the AppImage
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Args, Clone, Debug)]
struct LegacyRunArgs {
    /// Path to the AppImage file (legacy mode)
    appimage: Option<PathBuf>,

    #[command(flatten)]
    guest: CommonGuestOpts,

    #[command(flatten)]
    extraction: ExtractionOpts,

    /// Write a JSON report (evidence artifact) describing what was executed and what was modified.
    #[arg(long)]
    report: Option<PathBuf>,

    /// Arguments to pass to the AppImage
    #[arg(last = true)]
    args: Vec<String>,
}

#[derive(Args, Clone, Debug)]
struct ProbeArgs {
    #[command(subcommand)]
    kind: ProbeKind,

    #[command(flatten)]
    guest: CommonGuestOpts,

    /// Output directory for evidence artifacts.
    ///
    /// If not provided, defaults to `docs/agent-context/research/<probe>/<timestamp>/`.
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

#[derive(Subcommand, Clone, Debug)]
enum ProbeKind {
    /// Capture guest display-related environment and basic X11 info
    Display,

    /// Capture guest GPU renderer details (best-effort)
    Gpu,

    /// Capture guest device visibility (USB/hidraw) for debugging passthrough
    Devices,

    /// Capture X11 extension opcode mappings (to identify "major code" values)
    X11Opcodes,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum ExtractWith {
    Auto,
    Unsquashfs,
    SquashfsNg,
}

struct PcscBridgeGuard {
    enabled: bool,
    host_port: u32,
    guest_socket: PathBuf,
    runner_exe: PathBuf,
    host_link_path: Option<PathBuf>,
}

impl PcscBridgeGuard {
    fn disabled() -> Self {
        Self {
            enabled: false,
            host_port: 0,
            guest_socket: PathBuf::new(),
            runner_exe: PathBuf::new(),
            host_link_path: None,
        }
    }

    fn apply_env(&self, envs: &[String]) -> Vec<String> {
        if !self.enabled {
            return envs.to_vec();
        }

        let mut out = envs.to_vec();
        out.push(format!(
            "PCSCLITE_CSOCK_NAME={}",
            self.guest_socket.display()
        ));
        out
    }

    fn apply_guest_pre(&self, user_pre: Option<&str>) -> Option<String> {
        if !self.enabled {
            return user_pre.map(|s| s.to_string());
        }

        let guest_runner = format!("/run/muvm-host{}", self.runner_exe.display());
        let prelude = format!(
            r#"# pcsc bridge (guest)
export PCSCLITE_CSOCK_NAME="{sock}"
rm -f "$PCSCLITE_CSOCK_NAME" || true
"{runner}" pcsc-guest --host-port {port} --listen "$PCSCLITE_CSOCK_NAME" >/tmp/pcsc-guest.log 2>&1 &
for i in $(seq 1 50); do
    [ -S "$PCSCLITE_CSOCK_NAME" ] && break
    sleep 0.05
done
ls -l "$PCSCLITE_CSOCK_NAME" || true
"#,
            sock = self.guest_socket.display(),
            runner = guest_runner,
            port = self.host_port,
        );

        match user_pre {
            Some(user) => Some(format!("{prelude}\n{user}")),
            None => Some(prelude),
        }
    }

    fn shutdown(self) {
        if let Some(path) = self.host_link_path {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn maybe_enable_pcsc_bridge(
    opts: &CommonGuestOpts,
    out_dir: Option<&Path>,
) -> Result<PcscBridgeGuard> {
    if !opts.pcsc_bridge {
        return Ok(PcscBridgeGuard::disabled());
    }

    // muvm/libkrun does not provide arbitrary guest->host AF_VSOCK routing.
    // Instead, muvm registers a dynamic range of vsock ports (50000..50200) which connect
    // to host UNIX socket paths under $XDG_RUNTIME_DIR/krun/socket/port-<port>.
    // We create a symlink at that path pointing to the host pcscd socket.
    let run_dir = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR not set")?;
    let socket_dir = Path::new(&run_dir).join("krun/socket");
    std::fs::create_dir_all(&socket_dir)
        .with_context(|| format!("create {}", socket_dir.display()))?;

    let link_path = socket_dir.join(format!("port-{}", opts.pcsc_vsock_port));
    if link_path.exists() {
        // Avoid clobbering something muvm (or another app) already set up.
        let meta = std::fs::symlink_metadata(&link_path)
            .with_context(|| format!("stat {}", link_path.display()))?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&link_path)
                .with_context(|| format!("readlink {}", link_path.display()))?;
            if target != opts.pcsc_host_socket {
                anyhow::bail!(
                    "PC/SC bridge port {} is already in use ({} -> {}). Choose a different --pcsc-vsock-port.",
                    opts.pcsc_vsock_port,
                    link_path.display(),
                    target.display()
                );
            }
        } else {
            anyhow::bail!(
                "PC/SC bridge port {} path already exists and is not a symlink: {}",
                opts.pcsc_vsock_port,
                link_path.display()
            );
        }
    } else {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&opts.pcsc_host_socket, &link_path).with_context(|| {
                format!(
                    "symlink {} -> {}",
                    link_path.display(),
                    opts.pcsc_host_socket.display()
                )
            })?;
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("pcsc bridge requires unix")
        }
    }

    if let Some(dir) = out_dir {
        let log_path = dir.join("pcsc-host.log");
        let msg = format!(
            "pcsc-bridge(host): link {} -> {}\n",
            link_path.display(),
            opts.pcsc_host_socket.display()
        );
        let _ = std::fs::write(&log_path, msg);
    }

    let runner_exe = std::env::current_exe().context("current_exe")?;
    let runner_exe = runner_exe
        .canonicalize()
        .unwrap_or_else(|_| runner_exe.clone());

    Ok(PcscBridgeGuard {
        enabled: true,
        host_port: opts.pcsc_vsock_port,
        guest_socket: opts.pcsc_guest_socket.clone(),
        runner_exe,
        host_link_path: Some(link_path),
    })
}

// ---- PC/SC bridge (best-effort) ----

const VMADDR_CID_HOST: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct SockAddrVm {
    svm_family: libc::sa_family_t,
    svm_reserved1: libc::c_ushort,
    svm_port: u32,
    svm_cid: u32,
    svm_zero: [u8; 4],
}

fn pcsc_bridge_host_listen(vsock_port: u32, pcsc_socket: &Path) -> Result<()> {
    let listener_fd = vsock_listen(vsock_port)?;
    eprintln!(
        "pcsc-bridge(host): listening on vsock port {vsock_port}, forwarding to {}",
        pcsc_socket.display()
    );

    loop {
        let (client_fd, peer_cid, peer_port) = vsock_accept(listener_fd)?;
        let pcsc_socket = pcsc_socket.to_path_buf();
        std::thread::spawn(move || {
            if let Err(err) = pcsc_bridge_host_handle(client_fd, peer_cid, peer_port, &pcsc_socket)
            {
                eprintln!("pcsc-bridge(host): client error: {err:#}");
            }
        });
    }
}

fn pcsc_bridge_host_handle(
    client_fd: OwnedFd,
    peer_cid: u32,
    peer_port: u32,
    pcsc_socket: &Path,
) -> Result<()> {
    eprintln!("pcsc-bridge(host): accepted from cid={peer_cid} port={peer_port}");

    let unix = std::os::unix::net::UnixStream::connect(pcsc_socket)
        .with_context(|| format!("connect to host pcsc socket: {}", pcsc_socket.display()))?;

    let client = unsafe { File::from_raw_fd(client_fd.into_raw_fd()) };
    bidir_copy_unix_file(unix, client)
}

fn pcsc_bridge_guest_listen(listen_path: &Path, host_port: u32) -> Result<()> {
    if let Some(parent) = listen_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }

    // Remove any stale socket file.
    let _ = std::fs::remove_file(listen_path);

    let listener = std::os::unix::net::UnixListener::bind(listen_path)
        .with_context(|| format!("bind guest unix socket {}", listen_path.display()))?;
    eprintln!(
        "pcsc-bridge(guest): listening on {}, forwarding to host vsock port {host_port}",
        listen_path.display()
    );

    for stream in listener.incoming() {
        let stream = stream.context("accept guest unix client")?;
        std::thread::spawn(move || {
            if let Err(err) = pcsc_bridge_guest_handle(stream, host_port) {
                eprintln!("pcsc-bridge(guest): client error: {err:#}");
            }
        });
    }
    Ok(())
}

fn pcsc_bridge_guest_handle(unix: std::os::unix::net::UnixStream, host_port: u32) -> Result<()> {
    eprintln!(
        "pcsc-bridge(guest): accepted unix client, connecting to host vsock port {host_port}"
    );
    let vsock_fd = vsock_connect(VMADDR_CID_HOST, host_port)
        .with_context(|| format!("connect vsock host port {host_port}"))?;

    let vsock = unsafe { File::from_raw_fd(vsock_fd.into_raw_fd()) };
    bidir_copy_unix_file(unix, vsock)
}

fn bidir_copy_unix_file(unix: std::os::unix::net::UnixStream, file: File) -> Result<()> {
    let mut unix_a = unix;
    let mut unix_b = unix_a.try_clone().context("clone unix stream")?;

    let mut file_a = file;
    let mut file_b = file_a.try_clone().context("clone vsock fd")?;

    let t1 = std::thread::spawn(move || -> Result<()> {
        std::io::copy(&mut unix_a, &mut file_a).context("copy unix->vsock")?;
        Ok(())
    });

    let t2 = std::thread::spawn(move || -> Result<()> {
        std::io::copy(&mut file_b, &mut unix_b).context("copy vsock->unix")?;
        Ok(())
    });

    t1.join()
        .map_err(|_| anyhow::anyhow!("copy thread 1 panicked"))??;
    t2.join()
        .map_err(|_| anyhow::anyhow!("copy thread 2 panicked"))??;
    Ok(())
}

fn vsock_listen(port: u32) -> Result<RawFd> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("socket(AF_VSOCK)");
    }

    let addr = SockAddrVm {
        svm_family: libc::AF_VSOCK as libc::sa_family_t,
        svm_reserved1: 0,
        svm_port: port,
        svm_cid: libc::VMADDR_CID_ANY,
        svm_zero: [0; 4],
    };

    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const SockAddrVm as *const libc::sockaddr,
            std::mem::size_of::<SockAddrVm>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err).context("bind(vsock)");
    }

    let rc = unsafe { libc::listen(fd, 128) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err).context("listen(vsock)");
    }

    Ok(fd)
}

fn vsock_accept(listener_fd: RawFd) -> Result<(OwnedFd, u32, u32)> {
    let mut addr = SockAddrVm {
        svm_family: libc::AF_VSOCK as libc::sa_family_t,
        svm_reserved1: 0,
        svm_port: 0,
        svm_cid: 0,
        svm_zero: [0; 4],
    };
    let mut len = std::mem::size_of::<SockAddrVm>() as libc::socklen_t;
    let fd = unsafe {
        libc::accept(
            listener_fd,
            &mut addr as *mut SockAddrVm as *mut libc::sockaddr,
            &mut len,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("accept(vsock)");
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok((owned, addr.svm_cid, addr.svm_port))
}

fn vsock_connect(cid: u32, port: u32) -> Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("socket(AF_VSOCK)");
    }
    let addr = SockAddrVm {
        svm_family: libc::AF_VSOCK as libc::sa_family_t,
        svm_reserved1: 0,
        svm_port: port,
        svm_cid: cid,
        svm_zero: [0; 4],
    };
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const SockAddrVm as *const libc::sockaddr,
            std::mem::size_of::<SockAddrVm>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(err).context("connect(vsock)");
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Run(args)) => run_mode(args),
        Some(Commands::Probe(args)) => probe_mode(args),
        Some(Commands::PcscHost(args)) => pcsc_host_mode(args),
        Some(Commands::PcscGuest(args)) => pcsc_guest_mode(args),
        None => legacy_mode(cli.legacy),
    }
}

fn pcsc_host_mode(args: PcscHostArgs) -> Result<()> {
    pcsc_bridge_host_listen(args.port, &args.pcsc_socket)
}

fn pcsc_guest_mode(args: PcscGuestArgs) -> Result<()> {
    pcsc_bridge_guest_listen(&args.listen, args.host_port)
}

fn legacy_mode(args: LegacyRunArgs) -> Result<()> {
    let Some(appimage) = args.appimage else {
        anyhow::bail!("missing APPIMAGE (try: appimage-runner run <AppImage> ...)");
    };

    let appimage_path = appimage
        .canonicalize()
        .context("Failed to canonicalize AppImage path")?;
    let muvm_path = canonicalize_muvm_path(&args.guest.muvm_path)?;

    validate_muvm_args(&muvm_path, &args.guest.muvm_arg)?;

    println!("Getting offset for: {}", appimage_path.display());
    let offset = get_offset(&appimage_path)?;
    println!("Detected offset: {}", offset);

    let extract_dir = extract_appimage(&appimage_path, offset, args.extraction.extract_with)?;
    println!("Extracted to: {}", extract_dir.display());

    let mut strip_report = StripReport::default();
    if args.extraction.strip_gnu_property {
        let objcopy = resolve_objcopy_path(args.extraction.objcopy_path.as_deref())
            .context("Resolving objcopy path")?;
        strip_report = strip_gnu_property_notes_in_appdir(&extract_dir, &objcopy)
            .context("Stripping .note.gnu.property inside extracted AppImage")?;
    }

    let (fex_images, fex_rootfs_compat_overlay) =
        prepare_fex_images(&args.guest.fex_image, args.guest.fex_profile)
            .context("Preparing FEX images")?;

    let pcsc = maybe_enable_pcsc_bridge(&args.guest, None)?;
    let effective_env = pcsc.apply_env(&args.guest.env);
    let effective_guest_pre = pcsc.apply_guest_pre(args.guest.guest_pre.as_deref());

    let (run_report, _combined) = run_appimage(
        &extract_dir,
        &args.args,
        &effective_env,
        &fex_images,
        &muvm_path,
        &args.guest.muvm_arg,
        args.guest.timeout_seconds,
        effective_guest_pre.as_deref(),
    )?;

    pcsc.shutdown();

    if let Some(path) = args.report.as_ref() {
        let report = RunnerReport {
            appimage: appimage_path.display().to_string(),
            extract_dir: extract_dir.display().to_string(),
            strip_gnu_property: args.extraction.strip_gnu_property,
            fex_images: fex_images.iter().map(|p| p.display().to_string()).collect(),
            fex_rootfs_compat_overlay,
            muvm_path: muvm_path.display().to_string(),
            muvm_args: args
                .guest
                .muvm_arg
                .iter()
                .map(|s| s.to_string_lossy().to_string())
                .collect(),
            entrypoint: run_report.entrypoint.clone(),
            muvm_exit_status: run_report.muvm_exit_status.clone(),
            muvm_succeeded: run_report.muvm_succeeded,
            muvm_guest_status_code: run_report.muvm_guest_status_code,
            muvm_guest_terminated_signal: run_report.muvm_guest_terminated_signal,
            timeout_seconds: args.guest.timeout_seconds,
            timed_out: run_report.timed_out,
            strip_report,
        };

        write_json(path, &report).with_context(|| format!("Writing report {}", path.display()))?;
        println!("Wrote report: {}", path.display());
    }

    exit_from_run_report(&run_report)
}

fn run_mode(args: RunArgs) -> Result<()> {
    let appimage_path = args
        .appimage
        .canonicalize()
        .context("Failed to canonicalize AppImage path")?;
    let muvm_path = canonicalize_muvm_path(&args.guest.muvm_path)?;

    validate_muvm_args(&muvm_path, &args.guest.muvm_arg)?;

    let app_name = appimage_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "appimage".to_string());
    let out_dir = args.out_dir.unwrap_or_else(|| default_out_dir(&app_name));
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("Creating out dir {}", out_dir.display()))?;

    println!("Getting offset for: {}", appimage_path.display());
    let offset = get_offset(&appimage_path)?;
    println!("Detected offset: {}", offset);

    let extract_dir = extract_appimage(&appimage_path, offset, args.extraction.extract_with)?;
    println!("Extracted to: {}", extract_dir.display());

    let mut strip_report = StripReport::default();
    if args.extraction.strip_gnu_property {
        let objcopy = resolve_objcopy_path(args.extraction.objcopy_path.as_deref())
            .context("Resolving objcopy path")?;
        strip_report = strip_gnu_property_notes_in_appdir(&extract_dir, &objcopy)
            .context("Stripping .note.gnu.property inside extracted AppImage")?;
    }

    let (fex_images, fex_rootfs_compat_overlay) =
        prepare_fex_images(&args.guest.fex_image, args.guest.fex_profile)
            .context("Preparing FEX images")?;

    let pcsc = maybe_enable_pcsc_bridge(&args.guest, Some(&out_dir))?;
    let effective_env = pcsc.apply_env(&args.guest.env);
    let effective_guest_pre = pcsc.apply_guest_pre(args.guest.guest_pre.as_deref());

    let inputs = InputsReport {
        kind: "run".to_string(),
        appimage: Some(appimage_path.display().to_string()),
        extract_dir: Some(extract_dir.display().to_string()),
        fex_images: fex_images.iter().map(|p| p.display().to_string()).collect(),
        fex_rootfs_compat_overlay,
        muvm_path: muvm_path.display().to_string(),
        muvm_args: args
            .guest
            .muvm_arg
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect(),
        env: effective_env.clone(),
        timeout_seconds: args.guest.timeout_seconds,
        guest_pre: effective_guest_pre.clone(),
        argv_after_double_dash: Some(args.args.clone()),
    };

    let inputs_path = out_dir.join("inputs.json");
    write_json(&inputs_path, &inputs)
        .with_context(|| format!("Writing inputs {}", inputs_path.display()))?;

    let (run_report, combined) = run_appimage(
        &extract_dir,
        &args.args,
        &effective_env,
        &fex_images,
        &muvm_path,
        &args.guest.muvm_arg,
        args.guest.timeout_seconds,
        effective_guest_pre.as_deref(),
    )?;

    pcsc.shutdown();

    let log_path = out_dir.join("run.log");
    std::fs::write(&log_path, combined)
        .with_context(|| format!("Writing log {}", log_path.display()))?;

    let report = RunnerReport {
        appimage: appimage_path.display().to_string(),
        extract_dir: extract_dir.display().to_string(),
        strip_gnu_property: args.extraction.strip_gnu_property,
        fex_images: fex_images.iter().map(|p| p.display().to_string()).collect(),
        fex_rootfs_compat_overlay: inputs.fex_rootfs_compat_overlay.clone(),
        muvm_path: muvm_path.display().to_string(),
        muvm_args: inputs.muvm_args.clone(),
        entrypoint: run_report.entrypoint.clone(),
        muvm_exit_status: run_report.muvm_exit_status.clone(),
        muvm_succeeded: run_report.muvm_succeeded,
        muvm_guest_status_code: run_report.muvm_guest_status_code,
        muvm_guest_terminated_signal: run_report.muvm_guest_terminated_signal,
        timeout_seconds: args.guest.timeout_seconds,
        timed_out: run_report.timed_out,
        strip_report,
    };
    let report_path = out_dir.join("run.report.json");
    write_json(&report_path, &report)
        .with_context(|| format!("Writing report {}", report_path.display()))?;

    if let Some(path) = args.report.as_ref() {
        write_json(path, &report).with_context(|| format!("Writing report {}", path.display()))?;
    }

    println!("Wrote artifacts: {}", out_dir.display());
    exit_from_run_report(&run_report)
}

fn probe_mode(args: ProbeArgs) -> Result<()> {
    let muvm_path = canonicalize_muvm_path(&args.guest.muvm_path)?;

    validate_muvm_args(&muvm_path, &args.guest.muvm_arg)?;
    let probe_name = match args.kind {
        ProbeKind::Display => "probe-display",
        ProbeKind::Gpu => "probe-gpu",
        ProbeKind::Devices => "probe-devices",
        ProbeKind::X11Opcodes => "probe-x11-opcodes",
    };
    let out_dir = args.out_dir.unwrap_or_else(|| default_out_dir(probe_name));
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("Creating out dir {}", out_dir.display()))?;

    let (fex_images, fex_rootfs_compat_overlay) =
        prepare_fex_images(&args.guest.fex_image, args.guest.fex_profile)
            .context("Preparing FEX images")?;

    let pcsc = maybe_enable_pcsc_bridge(&args.guest, Some(&out_dir))?;
    let effective_env = pcsc.apply_env(&args.guest.env);
    let effective_guest_pre = pcsc.apply_guest_pre(args.guest.guest_pre.as_deref());

    let guest_cmd: String = match args.kind {
        ProbeKind::Display => r#"set -euo pipefail
echo '== env =='
env | sort | egrep '^(DISPLAY|XAUTHORITY|XDG_SESSION_TYPE|WAYLAND_DISPLAY|APPDIR)=' || true

echo '== x11 =='
if command -v xdpyinfo >/dev/null 2>&1; then
    xdpyinfo -display "${DISPLAY:-:1}" | sed -n '1,60p'
else
    echo 'xdpyinfo not present'
fi
"#
        .to_string(),
        ProbeKind::Gpu => r#"set -euo pipefail
echo '== glxinfo =='
if command -v glxinfo >/dev/null 2>&1; then
    glxinfo -B
else
    echo 'glxinfo not present'
fi

echo '== eglinfo =='
if command -v eglinfo >/dev/null 2>&1; then
    eglinfo | sed -n '1,120p'
else
    echo 'eglinfo not present'
fi

echo '== vulkaninfo =='
if command -v vulkaninfo >/dev/null 2>&1; then
    vulkaninfo --summary
else
    echo 'vulkaninfo not present'
fi
"#
        .to_string(),
        ProbeKind::Devices => r#"set -euo pipefail
echo '== whoami =='
id || true

echo '== env =='
env | sort | egrep '^(DISPLAY|XAUTHORITY|XDG_SESSION_TYPE|WAYLAND_DISPLAY)=' || true

echo '== /dev (high level) =='
ls -la /dev | sed -n '1,200p' || true

echo '== /dev/bus/usb =='
if [ -d /dev/bus/usb ]; then
    find /dev/bus/usb -maxdepth 2 -type c -o -type d 2>/dev/null | sort | sed -n '1,200p'
    ls -la /dev/bus/usb || true
    for d in /dev/bus/usb/*; do
        [ -d "$d" ] || continue
        echo "-- $d"
        ls -la "$d" || true
    done
else
    echo '/dev/bus/usb not present'
fi

echo '== hidraw =='
ls -la /dev/hidraw* 2>/dev/null || echo 'no /dev/hidraw*'

echo '== uhid =='
ls -la /dev/uhid 2>/dev/null || echo 'no /dev/uhid'

echo '== input =='
ls -la /dev/input 2>/dev/null || echo 'no /dev/input'

echo '== sysfs usb devices =='
if [ -d /sys/bus/usb/devices ]; then
    ls -la /sys/bus/usb/devices | sed -n '1,200p' || true
    for dev in /sys/bus/usb/devices/*; do
        [ -e "$dev" ] || continue
        base=$(basename "$dev")
        case "$base" in
            usb*|[0-9]-*|[0-9]-*.*)
                echo "-- $base"
                for f in idVendor idProduct manufacturer product serial speed busnum devnum; do
                    if [ -r "$dev/$f" ]; then
                        printf '%s=' "$f"; cat "$dev/$f"; echo
                    fi
                done
                ;;
        esac
    done
else
    echo '/sys/bus/usb/devices not present'
fi

echo '== pcsclite library presence (x86_64 rootfs via FEX) =='
(ldconfig -p || true) | grep -i pcsclite || true
ls -l /usr/lib64/libpcsclite.so.1* 2>/dev/null || true
"#
        .to_string(),
        ProbeKind::X11Opcodes => {
            // Run the host-built aarch64 helper inside the guest via muvm's host mount.
            // muvm mounts the host root at /run/muvm-host.
            let host_pwd = std::env::current_dir().context("get current dir")?;
            let helper_host_path = host_pwd.join("target").join("debug").join("x11-opcodes");
            let helper_host_path = helper_host_path
                .canonicalize()
                .unwrap_or_else(|_| helper_host_path.clone());
            let helper_guest_path = format!("/run/muvm-host{}", helper_host_path.display());

            format!(
                r#"set -euo pipefail
echo '== env =='
env | sort | egrep '^(DISPLAY|XAUTHORITY|XDG_SESSION_TYPE|WAYLAND_DISPLAY)=' || true

echo '== helper =='
HELPER='{helper_guest_path}'
if [ ! -x "$HELPER" ]; then
  echo "helper not executable at $HELPER"
  echo "expected you built it on the host with: cargo build -p x11-opcodes"
  ls -la "$(dirname "$HELPER")" || true
  exit 2
fi

"$HELPER"
"#
            )
        }
    };

    let inputs = InputsReport {
        kind: probe_name.to_string(),
        appimage: None,
        extract_dir: None,
        fex_images: fex_images.iter().map(|p| p.display().to_string()).collect(),
        fex_rootfs_compat_overlay,
        muvm_path: muvm_path.display().to_string(),
        muvm_args: args
            .guest
            .muvm_arg
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect(),
        env: effective_env.clone(),
        timeout_seconds: args.guest.timeout_seconds,
        guest_pre: effective_guest_pre.clone(),
        argv_after_double_dash: None,
    };
    let inputs_path = out_dir.join("inputs.json");
    write_json(&inputs_path, &inputs)
        .with_context(|| format!("Writing inputs {}", inputs_path.display()))?;

    let (status, combined, timed_out) = run_guest_command(
        &muvm_path,
        &inputs.muvm_args,
        &fex_images,
        &inputs.env,
        args.guest.timeout_seconds,
        inputs.guest_pre.as_deref(),
        &guest_cmd,
    )
    .context("Running probe")?;

    pcsc.shutdown();

    let log_path = out_dir.join("run.log");
    std::fs::write(&log_path, &combined)
        .with_context(|| format!("Writing log {}", log_path.display()))?;

    let muvm_guest_status_code = parse_muvm_guest_status_code(&combined);
    let muvm_guest_terminated_signal = parse_muvm_guest_terminated_signal(&combined);

    let report = ProbeReport {
        kind: inputs.kind.clone(),
        fex_images: inputs.fex_images.clone(),
        fex_rootfs_compat_overlay: inputs.fex_rootfs_compat_overlay.clone(),
        muvm_path: inputs.muvm_path.clone(),
        muvm_args: inputs.muvm_args.clone(),
        env: inputs.env.clone(),
        guest_pre: inputs.guest_pre.clone(),
        muvm_exit_status: format!("{:?}", status),
        muvm_succeeded: status.success(),
        muvm_guest_status_code,
        muvm_guest_terminated_signal,
        timeout_seconds: args.guest.timeout_seconds,
        timed_out,
    };
    let report_path = out_dir.join("run.report.json");
    write_json(&report_path, &report)
        .with_context(|| format!("Writing report {}", report_path.display()))?;
    println!("Wrote artifacts: {}", out_dir.display());

    if !status.success() {
        anyhow::bail!("muvm failed with status: {:?}", status);
    }
    Ok(())
}

fn canonicalize_muvm_path(muvm_path: &Path) -> Result<PathBuf> {
    if muvm_path.is_absolute() {
        Ok(muvm_path
            .canonicalize()
            .unwrap_or_else(|_| muvm_path.to_path_buf()))
    } else {
        // If the user gave a bare command name (e.g. `muvm`), do not join it with
        // the current directory: that would defeat PATH resolution.
        let mut comps = muvm_path.components();
        if matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none() {
            return Ok(muvm_path.to_path_buf());
        }

        let cwd = std::env::current_dir().context("Failed to get current directory")?;
        let joined = cwd.join(muvm_path);
        Ok(joined.canonicalize().unwrap_or(joined))
    }
}

fn validate_muvm_args(muvm_path: &Path, muvm_args: &[OsString]) -> Result<()> {
    // Some muvm builds support extra flags (e.g. gpu mode selection). Others will forward unknown
    // flags into the guest argv, which is confusing (e.g. `/bin/bash: --gpu-mode=...: invalid option`).
    //
    // Best-effort validation: if the user asks for a known muvm-only flag, ensure the selected muvm
    // binary advertises it in `--help`.
    let wants_gpu_mode = muvm_args
        .iter()
        .any(|a| a.to_string_lossy().starts_with("--gpu-mode"));
    if !wants_gpu_mode {
        return Ok(());
    }

    let out = Command::new(muvm_path)
        .arg("--help")
        .output()
        .with_context(|| format!("running {} --help", muvm_path.display()))?;
    let mut help = String::new();
    help.push_str(&String::from_utf8_lossy(&out.stdout));
    help.push_str(&String::from_utf8_lossy(&out.stderr));

    if !help.contains("--gpu-mode") {
        anyhow::bail!(
            "{} does not appear to support `--gpu-mode`. \
You may be using the system muvm; try `--muvm-path third_party/muvm/target/debug/muvm` (or another muvm build that supports GPU modes).",
            muvm_path.display()
        );
    }

    Ok(())
}

fn prepare_fex_images(
    images: &[PathBuf],
    profile: FexProfile,
) -> Result<(Vec<PathBuf>, Option<String>)> {
    let mut fex_images: Vec<PathBuf> = if images.is_empty() {
        discover_fex_images(profile).context("Discovering default FEX images")?
    } else {
        images
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
            .collect()
    };

    let mut fex_rootfs_compat_overlay: Option<String> = None;
    if let Some(overlay) =
        ensure_fex_rootfs_compat_overlay().context("Ensuring FEX RootFS compat overlay")?
    {
        let overlay = overlay
            .canonicalize()
            .unwrap_or_else(|_| overlay.to_path_buf());
        if !fex_images.iter().any(|p| p == &overlay) {
            fex_rootfs_compat_overlay = Some(overlay.display().to_string());
            fex_images.push(overlay);
        }
    }
    Ok((fex_images, fex_rootfs_compat_overlay))
}

fn discover_fex_images(profile: FexProfile) -> Result<Vec<PathBuf>> {
    let cwd = std::env::current_dir().context("Failed to get current directory")?;

    let candidates: Vec<&str> = match profile {
        FexProfile::Auto | FexProfile::Fedora => vec!["fedora-base-x86_64.erofs"],
        FexProfile::Sniper => vec!["sniper-sdk.erofs", "sniper.erofs", "sniper-debug.erofs"],
    };

    if profile == FexProfile::Sniper {
        for name in candidates {
            let path = cwd.join(name);
            if path.exists() {
                return Ok(vec![
                    path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
                ]);
            }
        }
        return Ok(vec![]);
    }

    // Fedora (and Auto's first choice): require or prefer fedora-base-x86_64.erofs.
    let fedora = cwd.join("fedora-base-x86_64.erofs");
    if fedora.exists() {
        return Ok(vec![
            fedora
                .canonicalize()
                .unwrap_or_else(|_| fedora.to_path_buf()),
        ]);
    }

    if profile == FexProfile::Fedora {
        anyhow::bail!(
            "--fex-profile=fedora requires ./fedora-base-x86_64.erofs in the current directory (or pass --fex-image ...)"
        );
    }

    // Auto fallback: try common sniper image names.
    for name in ["sniper-sdk.erofs", "sniper.erofs", "sniper-debug.erofs"] {
        let path = cwd.join(name);
        if path.exists() {
            return Ok(vec![
                path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
            ]);
        }
    }

    Ok(vec![])
}

fn default_out_dir(name: &str) -> PathBuf {
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    PathBuf::from("docs/agent-context/research")
        .join(sanitize_path_component(name))
        .join(ts)
}

fn sanitize_path_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn write_json<P: AsRef<Path>, T: Serialize>(path: P, value: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(value).context("Serializing JSON")?;
    std::fs::write(path.as_ref(), json)
        .with_context(|| format!("Writing {}", path.as_ref().display()))
}

fn exit_from_run_report(run_report: &RunReport) -> Result<()> {
    if !run_report.muvm_succeeded {
        anyhow::bail!("muvm failed with status: {}", run_report.muvm_exit_status);
    }
    if let Some(code) = run_report.muvm_guest_status_code {
        if code != 0 {
            anyhow::bail!("guest process exited with status code: {}", code);
        }
    }
    Ok(())
}

fn resolve_objcopy_path(explicit: Option<&Path>) -> Result<OsString> {
    if let Some(p) = explicit {
        return Ok(p.as_os_str().to_os_string());
    }

    fn works(candidate: &str, arg: &str) -> bool {
        Command::new(candidate)
            .arg(arg)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    for candidate in ["objcopy", "llvm-objcopy", "eu-objcopy"] {
        if works(candidate, "--version") || works(candidate, "-V") {
            return Ok(OsString::from(candidate));
        }
    }

    anyhow::bail!(
        "No usable objcopy found (tried: objcopy, llvm-objcopy, eu-objcopy). Install binutils (or llvm/eu-binutils) or pass --objcopy-path."
    )
}

fn get_offset(path: &Path) -> Result<u64> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut file = File::open(path).context("Failed to open AppImage")?;
    let mut buffer = [0u8; 4096];
    let mut pos = 0u64;

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read < 4 {
            break;
        }

        for i in 0..bytes_read - 3 {
            // Look for 'hsqs' (LE: 68 73 71 73)
            if buffer[i] == 0x68
                && buffer[i + 1] == 0x73
                && buffer[i + 2] == 0x71
                && buffer[i + 3] == 0x73
            {
                let candidate_offset = pos + i as u64;
                if verify_superblock(&mut file, candidate_offset)? {
                    return Ok(candidate_offset);
                }
                // Restore position to continue reading
                file.seek(SeekFrom::Start(pos + bytes_read as u64))?;
            }
        }

        if bytes_read < buffer.len() {
            break;
        }

        // Overlap: rewind 3 bytes to handle split magic
        pos += bytes_read as u64 - 3;
        file.seek(SeekFrom::Start(pos))?;
    }

    anyhow::bail!("SquashFS superblock not found");
}

fn verify_superblock(file: &mut std::fs::File, offset: u64) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;

    let mut sb = [0u8; 96];
    if file.read_exact(&mut sb).is_err() {
        return Ok(false);
    }

    // Magic check
    if sb[0] != 0x68 || sb[1] != 0x73 || sb[2] != 0x71 || sb[3] != 0x73 {
        return Ok(false);
    }

    // s_major is at offset 28 (2 bytes)
    let s_major = u16::from_le_bytes([sb[28], sb[29]]);
    if s_major != 4 {
        return Ok(false);
    }

    // s_block_size at offset 12 (4 bytes)
    let s_block_size = u32::from_le_bytes([sb[12], sb[13], sb[14], sb[15]]);
    if s_block_size == 0 || (s_block_size & (s_block_size - 1)) != 0 {
        return Ok(false);
    }

    Ok(true)
}

fn extract_appimage(path: &Path, offset: u64, extract_with: ExtractWith) -> Result<PathBuf> {
    // Determine cache directory
    let home = std::env::var("HOME").context("HOME not set")?;
    let cache_base = PathBuf::from(home).join(".cache/appimage-runner");

    // Use filename + simple hash of path for uniqueness
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    let hash = hasher.finish();

    let extract_dir = cache_base.join(format!("{}-{}", filename, hash));
    let squashfs_root = extract_dir.join("squashfs-root");

    if squashfs_root.exists() {
        // Assume already extracted
        // TODO: Check freshness?
        return Ok(squashfs_root);
    }

    std::fs::create_dir_all(&extract_dir).context("Failed to create cache dir")?;

    match extract_with {
        ExtractWith::Auto => {
            #[cfg(feature = "squashfs-ng")]
            {
                extract_appimage_squashfs_ng(path, offset, &extract_dir, &squashfs_root)
                    .context("extract via squashfs-ng")?;
                return Ok(squashfs_root);
            }

            #[cfg(not(feature = "squashfs-ng"))]
            {
                extract_appimage_unsquashfs(path, offset, &squashfs_root)
                    .context("extract via unsquashfs")?;
            }
        }
        ExtractWith::Unsquashfs => {
            extract_appimage_unsquashfs(path, offset, &squashfs_root)
                .context("extract via unsquashfs")?;
        }
        ExtractWith::SquashfsNg => {
            #[cfg(feature = "squashfs-ng")]
            {
                extract_appimage_squashfs_ng(path, offset, &extract_dir, &squashfs_root)
                    .context("extract via squashfs-ng")?;
            }

            #[cfg(not(feature = "squashfs-ng"))]
            {
                anyhow::bail!(
                    "--extract-with=squashfs-ng requires building with Cargo feature `squashfs-ng`"
                );
            }
        }
    }

    Ok(squashfs_root)
}

fn extract_appimage_unsquashfs(path: &Path, offset: u64, squashfs_root: &Path) -> Result<()> {
    // Run unsquashfs
    // unsquashfs -no-xattrs -o <offset> -d <dest> <path>
    let status = Command::new("unsquashfs")
        .arg("-no-xattrs")
        .arg("-o")
        .arg(offset.to_string())
        .arg("-d")
        .arg(squashfs_root)
        .arg(path)
        .status()
        .context("Failed to run unsquashfs")?;

    if !status.success() {
        anyhow::bail!("unsquashfs failed");
    }
    Ok(())
}

#[cfg(feature = "squashfs-ng")]
fn extract_appimage_squashfs_ng(
    appimage_path: &Path,
    offset: u64,
    extract_dir: &Path,
    squashfs_root: &Path,
) -> Result<()> {
    use anyhow::anyhow;
    use squashfs_ng::read::{Archive, Data};
    use std::fs::File;
    use std::io::{Seek, SeekFrom};

    std::fs::create_dir_all(squashfs_root).context("create squashfs-root")?;

    // squashfs-ng can only open archives by path and expects the superblock at file offset 0.
    // AppImages embed SquashFS at a non-zero offset, so we copy the SquashFS payload into a
    // cache file and then traverse/extract using squashfs-ng.
    let sfs_path = extract_dir.join("embedded.squashfs");
    if !sfs_path.exists() {
        let bytes_used = read_squashfs_bytes_used(appimage_path, offset)
            .context("read bytes_used from squashfs superblock")?;

        let mut src = File::open(appimage_path)
            .with_context(|| format!("open {}", appimage_path.display()))?;
        src.seek(SeekFrom::Start(offset))
            .context("seek to squashfs offset")?;

        let mut dst =
            File::create(&sfs_path).with_context(|| format!("create {}", sfs_path.display()))?;

        let mut limited = src.take(bytes_used);
        std::io::copy(&mut limited, &mut dst)
            .with_context(|| format!("copy squashfs payload to {}", sfs_path.display()))?;
    }

    let archive =
        Archive::open(&sfs_path).with_context(|| format!("open {}", sfs_path.display()))?;

    let mut hardlinks: HashMap<u32, PathBuf> = HashMap::new();
    let root = archive.get_exists("/").context("get squashfs root")?;

    fn dest_for_node(dest_root: &Path, node: &squashfs_ng::read::Node<'_>) -> Result<PathBuf> {
        let p = node
            .path()
            .ok_or_else(|| anyhow!("node missing path information"))?;
        if p == Path::new("/") {
            return Ok(dest_root.to_path_buf());
        }
        // Node paths are absolute; make them relative to the extraction root.
        let rel = p.strip_prefix("/").unwrap_or(p);
        Ok(dest_root.join(rel))
    }

    fn set_mode(path: &Path, mode: u16) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perm = std::fs::Permissions::from_mode((mode & 0o7777) as u32);
            std::fs::set_permissions(path, perm)
                .with_context(|| format!("set permissions on {}", path.display()))?;
        }
        let _ = mode;
        Ok(())
    }

    fn extract_node(
        dest_root: &Path,
        node: squashfs_ng::read::Node<'_>,
        hardlinks: &mut HashMap<u32, PathBuf>,
    ) -> Result<()> {
        use std::io::Write;

        let mode = node.mode();
        let id = node.id();
        let dest = dest_for_node(dest_root, &node)?;

        match node.data()? {
            Data::Dir(mut dir) => {
                std::fs::create_dir_all(&dest)
                    .with_context(|| format!("create dir {}", dest.display()))?;

                while let Some(child) = dir.next() {
                    extract_node(dest_root, child?, hardlinks)?;
                }

                set_mode(&dest, mode)?;
                Ok(())
            }
            Data::File(_) => {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create parent dir {}", parent.display()))?;
                }

                if let Some(existing) = hardlinks.get(&id) {
                    if std::fs::hard_link(existing, &dest).is_ok() {
                        set_mode(&dest, mode)?;
                        return Ok(());
                    }
                    // If hardlinking fails (e.g., cross-device), fall back to copy.
                }

                let mut src = node.as_file().context("open squashfs file")?;
                let mut dst = std::fs::File::create(&dest)
                    .with_context(|| format!("create file {}", dest.display()))?;
                std::io::copy(&mut src, &mut dst)
                    .with_context(|| format!("copy file data to {}", dest.display()))?;
                dst.flush().ok();
                set_mode(&dest, mode)?;

                hardlinks.entry(id).or_insert(dest);
                Ok(())
            }
            Data::Symlink(target) => {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create parent dir {}", parent.display()))?;
                }

                #[cfg(unix)]
                {
                    use std::os::unix::fs::symlink;
                    // Best-effort: if the path exists (e.g., reruns), replace it.
                    let _ = std::fs::remove_file(&dest);
                    let _ = std::fs::remove_dir(&dest);
                    symlink(&target, &dest)
                        .with_context(|| format!("symlink {} -> {:?}", dest.display(), target))?;
                    return Ok(());
                }

                #[cfg(not(unix))]
                {
                    anyhow::bail!("symlink extraction requires unix")
                }
            }
            other => {
                anyhow::bail!(
                    "Unsupported SquashFS node type '{}' at {:?}",
                    other.name(),
                    node.path()
                );
            }
        }
    }

    extract_node(squashfs_root, root, &mut hardlinks).context("extract archive")?;
    Ok(())
}

#[cfg(feature = "squashfs-ng")]
fn read_squashfs_bytes_used(appimage_path: &Path, offset: u64) -> Result<u64> {
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};

    let mut f =
        File::open(appimage_path).with_context(|| format!("open {}", appimage_path.display()))?;
    f.seek(SeekFrom::Start(offset))
        .context("seek to squashfs superblock")?;

    let mut sb = [0u8; 96];
    f.read_exact(&mut sb)
        .with_context(|| format!("read squashfs superblock at {}", offset))?;

    // bytes_used: u64 at offset 40 in the SquashFS v4 superblock.
    let bytes_used = u64::from_le_bytes(sb[40..48].try_into().unwrap());
    if bytes_used == 0 {
        anyhow::bail!("squashfs superblock bytes_used is 0")
    }
    Ok(bytes_used)
}

fn run_appimage(
    extract_dir: &Path,
    args: &[String],
    envs: &[String],
    fex_images: &[PathBuf],
    muvm_path: &Path,
    muvm_args: &[OsString],
    timeout_seconds: Option<u64>,
    guest_pre: Option<&str>,
) -> Result<(RunReport, String)> {
    let apprun = extract_dir.join("AppRun");

    // Some AppImages ship AppRun as a script (e.g. #!/bin/bash). muvm+FEX expects an ELF
    // entrypoint, so detect scripts and run them via their interpreter explicitly.
    let resolved = resolve_entrypoint(&apprun)
        .with_context(|| format!("Resolving AppRun entrypoint: {}", apprun.display()))?;
    let entry = resolved.entry.clone();
    let entry_args = resolved.entry_args.clone();

    // Construct argv for muvm.
    // NOTE: muvm's guest output is not reliably capturable via plain stdout/stderr pipes.
    // Run under a PTY so we can capture/parse diagnostics deterministically.
    let mut argv: Vec<String> = Vec::new();

    // Pass-through muvm arguments (e.g. --gpu-mode=drm).
    // Important: muvm is order-sensitive for some flags, and expects them before `--emu=fex`.
    argv.extend(muvm_args.iter().map(|s| s.to_string_lossy().to_string()));

    argv.push("--emu=fex".to_string());

    for img in fex_images {
        argv.push("--fex-image".to_string());
        argv.push(img.display().to_string());
    }

    // Set APPDIR (Required by AppImage spec)
    argv.push("-e".to_string());
    argv.push(format!("APPDIR={}", extract_dir.display()));

    // Pass user-provided envs
    for env in envs {
        argv.push("-e".to_string());
        argv.push(env.clone());
    }

    argv.push("--".to_string());

    if let Some(pre) = guest_pre {
        // Run an inline prelude in the guest before executing the AppImage entrypoint.
        // We avoid writing any wrapper scripts into the extracted AppImage directory.
        //
        // bash -lc '<pre>; exec "$@"' bash <entry> <entry_args...> <args...>
        argv.push("/bin/bash".to_string());
        argv.push("-lc".to_string());
        argv.push(format!("set -euo pipefail\n{}\nexec \"$@\"", pre));
        argv.push("bash".to_string());
        argv.push(entry.display().to_string());
        argv.extend(entry_args);
        argv.extend(args.iter().cloned());
    } else {
        argv.push(entry.display().to_string());
        argv.extend(entry_args);
        argv.extend(args.iter().cloned());
    }

    let timeout = timeout_seconds.map(Duration::from_secs);
    let (status, combined, timed_out) = run_in_pty(muvm_path, &argv, timeout)
        .with_context(|| format!("Failed to run AppRun via muvm ({})", muvm_path.display()))?;
    let muvm_guest_status_code = parse_muvm_guest_status_code(&combined);
    let muvm_guest_terminated_signal = parse_muvm_guest_terminated_signal(&combined);

    Ok((
        RunReport {
            entrypoint: resolved,
            muvm_exit_status: format!("{:?}", status),
            muvm_succeeded: status.success(),
            muvm_guest_status_code,
            muvm_guest_terminated_signal,
            timed_out,
        },
        combined,
    ))
}

fn run_guest_command(
    muvm_path: &Path,
    muvm_args: &[String],
    fex_images: &[PathBuf],
    envs: &[String],
    timeout_seconds: Option<u64>,
    guest_pre: Option<&str>,
    guest_cmd: &str,
) -> Result<(portable_pty::ExitStatus, String, bool)> {
    let mut argv: Vec<String> = Vec::new();

    // muvm is order-sensitive for some flags; put pass-through args first.
    argv.extend(muvm_args.iter().cloned());
    argv.push("--emu=fex".to_string());
    for img in fex_images {
        argv.push("--fex-image".to_string());
        argv.push(img.display().to_string());
    }
    for env in envs {
        argv.push("-e".to_string());
        argv.push(env.clone());
    }
    argv.push("--".to_string());

    let script = if let Some(pre) = guest_pre {
        format!("set -euo pipefail\n{}\n{}\n", pre, guest_cmd)
    } else {
        format!("{}\n", guest_cmd)
    };

    argv.push("/bin/bash".to_string());
    argv.push("-lc".to_string());
    argv.push(script);

    let timeout = timeout_seconds.map(Duration::from_secs);
    run_in_pty(muvm_path, &argv, timeout).with_context(|| {
        format!(
            "Failed to run guest command via muvm ({})",
            muvm_path.display()
        )
    })
}

#[derive(Serialize)]
struct InputsReport {
    kind: String,
    appimage: Option<String>,
    extract_dir: Option<String>,
    fex_images: Vec<String>,
    fex_rootfs_compat_overlay: Option<String>,
    muvm_path: String,
    muvm_args: Vec<String>,
    env: Vec<String>,
    timeout_seconds: Option<u64>,
    guest_pre: Option<String>,
    argv_after_double_dash: Option<Vec<String>>,
}

#[derive(Serialize)]
struct ProbeReport {
    kind: String,
    fex_images: Vec<String>,
    fex_rootfs_compat_overlay: Option<String>,
    muvm_path: String,
    muvm_args: Vec<String>,
    env: Vec<String>,
    guest_pre: Option<String>,
    muvm_exit_status: String,
    muvm_succeeded: bool,
    muvm_guest_status_code: Option<i32>,
    muvm_guest_terminated_signal: Option<i32>,
    timeout_seconds: Option<u64>,
    timed_out: bool,
}

fn run_in_pty(
    program: &Path,
    args: &[String],
    timeout: Option<Duration>,
) -> Result<(portable_pty::ExitStatus, String, bool)> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::sync::mpsc;
    use std::thread;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;

    let mut cmd = CommandBuilder::new(program.as_os_str());
    for a in args {
        cmd.arg(a);
    }

    let mut child = pair.slave.spawn_command(cmd).context("spawn_command")?;
    let mut killer = child.clone_killer();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().context("try_clone_reader")?;
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>>>();
    let reader_thread = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let _ = tx.send(Err(e).context("pty read"));
                    break;
                }
            }
        }
    });

    let mut output: Vec<u8> = Vec::new();
    let started = std::time::Instant::now();
    let mut timed_out = false;

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(chunk)) => {
                output.extend_from_slice(&chunk);
                // Stream output live (best-effort). PTY multiplexes stdout+stderr.
                let text = String::from_utf8_lossy(&chunk);
                print!("{}", text);
                let _ = std::io::stdout().flush();
            }
            Ok(Err(e)) => return Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        if let Some(max) = timeout {
            if !timed_out && started.elapsed() >= max {
                timed_out = true;
                let _ = killer.kill();
            }
        }

        if let Some(status) = child.try_wait().context("try_wait")? {
            let _ = reader_thread.join();
            return Ok((
                status,
                String::from_utf8_lossy(&output).to_string(),
                timed_out,
            ));
        }
    }
}

fn parse_muvm_guest_status_code(text: &str) -> Option<i32> {
    // muvm formats this like:
    //   "..." process exited with status code: 248
    // Capture the last occurrence to reflect the final guest exit.
    let needle = "process exited with status code:";
    let mut last: Option<i32> = None;
    for line in text.lines() {
        if let Some(idx) = line.find(needle) {
            let tail = line[idx + needle.len()..].trim();
            if let Ok(n) = tail.parse::<i32>() {
                last = Some(n);
            }
        }
    }
    last
}

fn parse_muvm_guest_terminated_signal(text: &str) -> Option<i32> {
    // muvm formats this like:
    //   "..." process terminated by signal: 11
    // Capture the last occurrence to reflect the final guest termination.
    let needle = "process terminated by signal:";
    let mut last: Option<i32> = None;
    for line in text.lines() {
        if let Some(idx) = line.find(needle) {
            let tail = line[idx + needle.len()..].trim();
            if let Ok(n) = tail.parse::<i32>() {
                last = Some(n);
            }
        }
    }
    last
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind")]
enum EntrypointKind {
    Elf,
    Script { interpreter: String },
}

#[derive(Clone, Debug, Serialize)]
struct ResolvedEntrypoint {
    apprun: String,
    entry: PathBuf,
    entry_args: Vec<String>,
    kind: EntrypointKind,
}

#[derive(Debug, Serialize)]
struct RunReport {
    entrypoint: ResolvedEntrypoint,
    muvm_exit_status: String,
    muvm_succeeded: bool,
    muvm_guest_status_code: Option<i32>,
    muvm_guest_terminated_signal: Option<i32>,
    timed_out: bool,
}

#[derive(Default, Debug, Serialize)]
struct StripReport {
    stripped_files: Vec<String>,
    strip_failures: Vec<StripFailure>,
    remaining_gnu_property_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct StripFailure {
    path: String,
    error: String,
}

#[derive(Debug, Serialize)]
struct RunnerReport {
    appimage: String,
    extract_dir: String,
    strip_gnu_property: bool,
    fex_images: Vec<String>,
    fex_rootfs_compat_overlay: Option<String>,
    muvm_path: String,
    muvm_args: Vec<String>,
    entrypoint: ResolvedEntrypoint,
    muvm_exit_status: String,
    muvm_succeeded: bool,
    muvm_guest_status_code: Option<i32>,
    muvm_guest_terminated_signal: Option<i32>,
    timeout_seconds: Option<u64>,
    timed_out: bool,
    strip_report: StripReport,
}

fn ensure_fex_rootfs_compat_overlay() -> Result<Option<PathBuf>> {
    #[cfg(not(unix))]
    {
        return Ok(None);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let home = std::env::var("HOME").context("HOME not set")?;
        let cache_base = PathBuf::from(home)
            .join(".cache")
            .join("appimage-runner")
            .join("fex-rootfs-compat");
        std::fs::create_dir_all(&cache_base).context("create fex-rootfs-compat cache dir")?;

        let overlay_path = cache_base.join("ldso-symlink-x86_64.erofs");
        if overlay_path.exists() {
            return Ok(Some(overlay_path));
        }

        let work_dir = cache_base.join("work");
        if work_dir.exists() {
            let _ = std::fs::remove_dir_all(&work_dir);
        }
        std::fs::create_dir_all(work_dir.join("lib64")).context("create overlay work dirs")?;

        // Provide /lib64/ld-linux-x86-64.so.2 by linking to the loader location in the system RootFS.
        // This matches the PT_INTERP path used by many x86_64 ELFs.
        symlink(
            "/usr/lib64/ld-linux-x86-64.so.2",
            work_dir.join("lib64").join("ld-linux-x86-64.so.2"),
        )
        .context("create ld-linux-x86-64.so.2 symlink")?;

        let status = Command::new("mkfs.erofs")
            .arg("-zlz4hc")
            .arg(&overlay_path)
            .arg(&work_dir)
            .status()
            .context("run mkfs.erofs")?;
        if !status.success() {
            anyhow::bail!("mkfs.erofs failed when building {}", overlay_path.display());
        }

        let _ = std::fs::remove_dir_all(&work_dir);
        Ok(Some(overlay_path))
    }
}

fn resolve_entrypoint(apprun: &Path) -> Result<ResolvedEntrypoint> {
    // If AppRun is a script with a shebang, run /path/to/interpreter [arg] AppRun.
    let data = std::fs::read(apprun).with_context(|| format!("read {}", apprun.display()))?;
    if data.starts_with(b"#!") {
        let line_end = data.iter().position(|&b| b == b'\n').unwrap_or(data.len());
        let line = String::from_utf8_lossy(&data[2..line_end])
            .trim()
            .to_string();
        let mut parts = line.split_whitespace();
        let interp = parts
            .next()
            .context("shebang missing interpreter path")?
            .to_string();
        let mut argv: Vec<String> = Vec::new();
        if let Some(arg) = parts.next() {
            argv.push(arg.to_string());
        }
        argv.push(apprun.display().to_string());
        return Ok(ResolvedEntrypoint {
            apprun: apprun.display().to_string(),
            entry: PathBuf::from(&interp),
            entry_args: argv,
            kind: EntrypointKind::Script {
                interpreter: interp,
            },
        });
    }

    Ok(ResolvedEntrypoint {
        apprun: apprun.display().to_string(),
        entry: apprun.to_path_buf(),
        entry_args: Vec::new(),
        kind: EntrypointKind::Elf,
    })
}

fn strip_gnu_property_notes_in_appdir(appdir: &Path, objcopy: &OsString) -> Result<StripReport> {
    let mut report = StripReport::default();

    // Conservative: only touch likely load-bearing executable/library locations.
    for rel in ["bin", "usr/bin", "usr/lib", "usr/lib64", "lib", "lib64"] {
        let dir = appdir.join(rel);
        if dir.exists() {
            strip_gnu_property_notes_in_tree(&dir, &mut report, objcopy)
                .with_context(|| format!("Stripping notes under {}", dir.display()))?;
        }
    }

    // Verify: collect any remaining x86_64 ELFs that still contain the note.
    for rel in ["bin", "usr/bin", "usr/lib", "usr/lib64", "lib", "lib64"] {
        let dir = appdir.join(rel);
        if !dir.exists() {
            continue;
        }
        collect_remaining_gnu_property_files(&dir, &mut report)
            .with_context(|| format!("Scanning remaining notes under {}", dir.display()))?;
    }

    report.stripped_files.sort();
    report.strip_failures.sort_by(|a, b| a.path.cmp(&b.path));
    report.remaining_gnu_property_files.sort();
    report.remaining_gnu_property_files.dedup();

    Ok(report)
}

fn strip_gnu_property_notes_in_tree(
    root: &Path,
    report: &mut StripReport,
    objcopy: &OsString,
) -> Result<()> {
    fn walk(dir: &Path, f: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)
                .with_context(|| format!("symlink_metadata {}", path.display()))?;
            if meta.is_dir() {
                walk(&path, f)?;
            } else if meta.is_file() {
                f(&path)?;
            }
        }
        Ok(())
    }

    walk(root, &mut |path| {
        if !is_elf_x86_64(path)? {
            return Ok(());
        }
        if !elf_has_section(path, b".note.gnu.property")? {
            return Ok(());
        }

        // objcopy edits the file in-place.
        let out = Command::new(objcopy)
            .arg("--remove-section")
            .arg(".note.gnu.property")
            .arg(path)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("objcopy on {}", path.display()))?;
        if !out.status.success() {
            // Don't hard-fail on a single file; keep going but surface stderr.
            report.strip_failures.push(StripFailure {
                path: path.display().to_string(),
                error: String::from_utf8_lossy(&out.stderr).to_string(),
            });
        } else {
            report.stripped_files.push(path.display().to_string());
        }
        Ok(())
    })
}

fn collect_remaining_gnu_property_files(root: &Path, report: &mut StripReport) -> Result<()> {
    fn walk(dir: &Path, f: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)
                .with_context(|| format!("symlink_metadata {}", path.display()))?;
            if meta.is_dir() {
                walk(&path, f)?;
            } else if meta.is_file() {
                f(&path)?;
            }
        }
        Ok(())
    }

    walk(root, &mut |path| {
        if !is_elf_x86_64(path)? {
            return Ok(());
        }
        if elf_has_section(path, b".note.gnu.property")? {
            report
                .remaining_gnu_property_files
                .push(path.display().to_string());
        }
        Ok(())
    })
}

fn is_elf_x86_64(path: &Path) -> Result<bool> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hdr = [0u8; 64];
    let n = f
        .read(&mut hdr)
        .with_context(|| format!("read {}", path.display()))?;
    if n < 20 {
        return Ok(false);
    }
    if &hdr[0..4] != b"\x7fELF" {
        return Ok(false);
    }
    // Only handle ELF64 little-endian here (fits our target).
    if hdr[4] != 2 || hdr[5] != 1 {
        return Ok(false);
    }
    let e_machine = u16::from_le_bytes([hdr[18], hdr[19]]);
    Ok(e_machine == 62)
}

fn elf_has_section(path: &Path, section_name: &[u8]) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;

    let mut ehdr = [0u8; 64];
    f.read_exact(&mut ehdr)
        .with_context(|| format!("read ELF header {}", path.display()))?;
    if &ehdr[0..4] != b"\x7fELF" {
        return Ok(false);
    }
    if ehdr[4] != 2 || ehdr[5] != 1 {
        return Ok(false);
    }

    let e_shoff = u64::from_le_bytes(ehdr[40..48].try_into().unwrap());
    let e_shentsize = u16::from_le_bytes(ehdr[58..60].try_into().unwrap()) as u64;
    let e_shnum = u16::from_le_bytes(ehdr[60..62].try_into().unwrap()) as u64;
    let e_shstrndx = u16::from_le_bytes(ehdr[62..64].try_into().unwrap()) as u64;
    if e_shoff == 0 || e_shentsize == 0 || e_shnum == 0 || e_shstrndx >= e_shnum {
        return Ok(false);
    }

    // Read the section header string table header.
    f.seek(SeekFrom::Start(e_shoff + e_shentsize * e_shstrndx))
        .with_context(|| format!("seek shstrndx {}", path.display()))?;
    let mut sh = vec![0u8; e_shentsize as usize];
    f.read_exact(&mut sh)
        .with_context(|| format!("read shstr header {}", path.display()))?;

    // sh_offset/sh_size in ELF64 section header: offsets 24..32, 32..40.
    let shstr_off = u64::from_le_bytes(sh[24..32].try_into().unwrap());
    let shstr_size = u64::from_le_bytes(sh[32..40].try_into().unwrap());
    if shstr_size == 0 {
        return Ok(false);
    }
    // Cap to something sane to avoid huge allocations on corrupt binaries.
    let cap = shstr_size.min(16 * 1024 * 1024);
    f.seek(SeekFrom::Start(shstr_off))
        .with_context(|| format!("seek shstrtab {}", path.display()))?;
    let mut shstr = vec![0u8; cap as usize];
    f.read_exact(&mut shstr)
        .with_context(|| format!("read shstrtab {}", path.display()))?;

    // Iterate section headers and compare names.
    for idx in 0..e_shnum {
        f.seek(SeekFrom::Start(e_shoff + e_shentsize * idx))
            .with_context(|| format!("seek section header {}", path.display()))?;
        f.read_exact(&mut sh)
            .with_context(|| format!("read section header {}", path.display()))?;
        let name_off = u32::from_le_bytes(sh[0..4].try_into().unwrap()) as usize;
        if name_off >= shstr.len() {
            continue;
        }
        let end = shstr[name_off..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| name_off + p)
            .unwrap_or(shstr.len());
        if &shstr[name_off..end] == section_name {
            return Ok(true);
        }
    }

    Ok(false)
}
