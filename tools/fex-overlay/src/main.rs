use anyhow::{Context, Result};
use clap::Parser;
use regex::Regex;
use serde::Serialize;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// List of packages to include in the overlay
    #[arg(required = true)]
    packages: Vec<String>,

    /// Output filename for the EROFS image
    #[arg(short, long, default_value = "overlay.erofs")]
    output: PathBuf,

    /// Fedora version to target (e.g., "42", "rawhide")
    #[arg(long, default_value = "rawhide")]
    fedora_version: String,

    /// Keep temporary files
    #[arg(long)]
    keep: bool,

    /// Write a JSON manifest describing downloaded/extracted/skipped RPMs
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Allow ABI-boundary components (loader/glibc/toolchain runtime) in the overlay.
    /// This is unsafe for deps overlays; use only for explicit "base refresh" work.
    #[arg(long)]
    allow_abi_boundary: bool,

    /// Strip the ELF .note.gnu.property section from x86_64 ELFs.
    /// Fedora packages may mark CET (IBT/SHSTK) via this note, which FEX can reject.
    #[arg(long, default_value_t = true)]
    strip_gnu_property: bool,
}

#[derive(Serialize)]
struct Manifest {
    fedora_version: String,
    repo_url: String,
    packages: Vec<String>,
    output: String,
    allow_abi_boundary: bool,
    strip_gnu_property: bool,
    downloaded_rpms: Vec<String>,
    extracted_rpms: Vec<String>,
    skipped_rpms: Vec<SkippedRpm>,
    stripped_elf_count: usize,
}

#[derive(Serialize)]
struct SkippedRpm {
    rpm: String,
    reason: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 1. Setup repo URL
    let repo_url = if cli.fedora_version == "rawhide" {
        "https://dl.fedoraproject.org/pub/fedora/linux/development/rawhide/Everything/x86_64/os/"
            .to_string()
    } else {
        format!(
            "https://dl.fedoraproject.org/pub/fedora/linux/releases/{}/Everything/x86_64/os/",
            cli.fedora_version
        )
    };

    println!("Targeting Fedora: {} ({})", cli.fedora_version, repo_url);

    // 2. Create temp dir
    let temp_dir = tempfile::Builder::new().prefix("fex-overlay-").tempdir()?;
    let work_dir = temp_dir.path();
    let rootfs_dir = work_dir.join("rootfs");
    std::fs::create_dir(&rootfs_dir)?;

    println!("Working in: {}", work_dir.display());

    // 3. Download RPMs (+ dependencies)
    let rpms_dir = work_dir.join("rpms");
    let rpms = download_rpms_with_deps(&cli.packages, &repo_url, &rpms_dir)?;

    // 4. Extract RPMs into staging tree (deps overlays must not alter ABI boundary)
    let deny_name_re = Regex::new(
        r"^(glibc|glibc-common|glibc-minimal-langpack|glibc-langpack|gcc-libs|libgcc|libstdc\+\+|libgomp|libatomic|libasan|libubsan)-",
    )
    .context("Failed to compile denylist regex")?;

    let mut extracted_rpms: Vec<String> = Vec::new();
    let mut skipped_rpms: Vec<SkippedRpm> = Vec::new();
    let mut downloaded_rpms: Vec<String> = Vec::new();

    for rpm_path in &rpms {
        downloaded_rpms.push(rpm_path.display().to_string());
    }

    for rpm_path in rpms {
        let rpm_filename = rpm_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        if !cli.allow_abi_boundary && deny_name_re.is_match(&rpm_filename) {
            skipped_rpms.push(SkippedRpm {
                rpm: rpm_filename,
                reason: "denylisted package family (ABI boundary)".to_string(),
            });
            continue;
        }

        if !cli.allow_abi_boundary {
            if let Some(reason) = rpm_forbidden_reason(&rpm_path)
                .with_context(|| format!("Checking forbidden paths in {}", rpm_path.display()))?
            {
                skipped_rpms.push(SkippedRpm {
                    rpm: rpm_filename,
                    reason,
                });
                continue;
            }
        }

        extract_rpm(&rpm_path, &rootfs_dir, cli.allow_abi_boundary)
            .with_context(|| format!("Extracting {}", rpm_path.display()))?;
        extracted_rpms.push(rpm_path.display().to_string());

        // Some RPMs create read-only directories (e.g. 0555). Later RPMs may need
        // to create files under those directories, so ensure the tree stays readable/writable
        // during the build.
        ensure_dirs_writable(&rootfs_dir)
            .with_context(|| format!("Normalizing perms after {}", rpm_path.display()))?;
    }

    // 5. Validate staging tree invariants before packing
    validate_staging_tree(&rootfs_dir, cli.allow_abi_boundary)
        .context("Staging tree failed invariants")?;

    // 6. Optional sanitization for FEX compatibility
    let stripped_elf_count = if cli.strip_gnu_property {
        strip_gnu_property_notes(&rootfs_dir).context("Stripping .note.gnu.property")?
    } else {
        0
    };

    // Re-validate after potential modifications.
    validate_staging_tree(&rootfs_dir, cli.allow_abi_boundary)
        .context("Staging tree failed invariants after sanitization")?;

    // 7. Pack EROFS
    println!("Packing EROFS image to: {}", cli.output.display());
    pack_erofs(&rootfs_dir, &cli.output)?;

    // 8. Emit manifest (evidence artifact)
    if let Some(path) = cli.manifest.as_ref() {
        let manifest = Manifest {
            fedora_version: cli.fedora_version.clone(),
            repo_url: repo_url.clone(),
            packages: cli.packages.clone(),
            output: cli.output.display().to_string(),
            allow_abi_boundary: cli.allow_abi_boundary,
            strip_gnu_property: cli.strip_gnu_property,
            downloaded_rpms,
            extracted_rpms,
            skipped_rpms,
            stripped_elf_count,
        };
        let json = serde_json::to_string_pretty(&manifest).context("Serializing manifest")?;
        std::fs::write(path, json)
            .with_context(|| format!("Writing manifest {}", path.display()))?;
        println!("Wrote manifest: {}", path.display());
    }

    if cli.keep {
        let path: PathBuf = temp_dir.keep();
        println!("Kept temporary directory: {}", path.display());
    }

    println!("Done!");
    Ok(())
}

fn download_rpms_with_deps(
    packages: &[String],
    repo_url: &str,
    destdir: &Path,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(destdir).context("Failed to create RPM download directory")?;

    // DNF5 supports dependency-resolving downloads.
    // We use --alldeps so the overlay doesn't accidentally rely on host-installed deps.
    let mut cmd = Command::new("dnf");
    cmd.arg(format!("--repofrompath=fedora-x86_64,{}", repo_url))
        .arg("--forcearch=x86_64")
        .arg("--assumeyes")
        .arg("--disablerepo=*")
        .arg("--enablerepo=fedora-x86_64")
        .arg("download")
        .arg("--arch=x86_64")
        .arg("--arch=noarch")
        .arg("--resolve")
        .arg("--alldeps")
        .arg(format!("--destdir={}", destdir.display()));

    for pkg in packages {
        cmd.arg(pkg);
    }

    let output = cmd
        .output()
        .context("Failed to run dnf download (with dependency resolution)")?;

    if !output.status.success() {
        anyhow::bail!(
            "dnf download failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut rpms = Vec::new();
    for entry in std::fs::read_dir(destdir).context("Failed to list downloaded RPMs")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rpm") {
            rpms.push(path);
        }
    }

    if rpms.is_empty() {
        anyhow::bail!(
            "dnf reported success but no RPMs were downloaded into {}",
            destdir.display()
        );
    }

    rpms.sort();
    Ok(rpms)
}

fn extract_rpm(rpm_path: &Path, dest_dir: &Path, allow_abi_boundary: bool) -> Result<()> {
    // rpm2cpio <rpm> | bsdtar -xf - -C <dest>
    // We use bsdtar so we can ignore archive permissions; cpio tends to apply
    // restrictive directory modes (e.g. 0555) early, which can break extraction.

    let mut rpm2cpio = Command::new("rpm2cpio")
        .arg(rpm_path)
        .stdout(Stdio::piped())
        .spawn()
        .context("Failed to spawn rpm2cpio")?;

    let mut bsdtar_cmd = Command::new("bsdtar");
    bsdtar_cmd
        .arg("-xf")
        .arg("-")
        .arg("-C")
        .arg(dest_dir)
        // Removes intervening directory symlinks instead of erroring.
        // This prevents libarchive's secure-symlink guard from aborting extraction.
        .arg("--unlink")
        .arg("--no-same-owner")
        .arg("--no-same-permissions");

    // Deps overlays should be library-focused and must not override base executables.
    // (e.g. AppRun uses #!/bin/bash; overriding bash can prevent the AppImage from starting.)
    if !allow_abi_boundary {
        bsdtar_cmd
            .arg("--exclude")
            .arg("./bin/*")
            .arg("--exclude")
            .arg("./sbin/*")
            .arg("--exclude")
            .arg("./usr/bin/*")
            .arg("--exclude")
            .arg("./usr/sbin/*");
    }

    let bsdtar = bsdtar_cmd
        .stdin(
            rpm2cpio
                .stdout
                .take()
                .context("rpm2cpio stdout was not piped")?,
        )
        .output()
        .context("Failed to run bsdtar")?;

    let rpm2cpio_status = rpm2cpio.wait().context("Failed to wait for rpm2cpio")?;

    if !rpm2cpio_status.success() {
        anyhow::bail!("rpm2cpio failed with status: {rpm2cpio_status}");
    }

    if !bsdtar.status.success() {
        anyhow::bail!("bsdtar failed: {}", String::from_utf8_lossy(&bsdtar.stderr));
    }

    Ok(())
}

fn rpm_forbidden_reason(rpm_path: &Path) -> Result<Option<String>> {
    // Conservative check: if an RPM payload includes ABI-boundary paths, we should
    // not put it in a deps overlay.
    //
    // This is intentionally simple and explainable; it can be expanded later.
    let forbidden_needles = [
        "./lib64/ld-linux-x86-64.so.2",
        "./usr/lib64/ld-linux-x86-64.so.2",
        "./lib64/libc.so.6",
        "./usr/lib64/libc.so.6",
        "./lib64/libstdc++.so.6",
        "./usr/lib64/libstdc++.so.6",
        "./lib64/libgcc_s.so.1",
        "./usr/lib64/libgcc_s.so.1",
    ];

    let mut rpm2cpio = Command::new("rpm2cpio")
        .arg(rpm_path)
        .stdout(Stdio::piped())
        .spawn()
        .context("Failed to spawn rpm2cpio")?;

    let list = Command::new("bsdtar")
        .arg("-tf")
        .arg("-")
        .stdin(
            rpm2cpio
                .stdout
                .take()
                .context("rpm2cpio stdout was not piped")?,
        )
        .output()
        .context("Failed to run bsdtar -tf")?;

    let rpm2cpio_status = rpm2cpio.wait().context("Failed to wait for rpm2cpio")?;

    if !rpm2cpio_status.success() {
        anyhow::bail!("rpm2cpio failed with status: {rpm2cpio_status}");
    }

    if !list.status.success() {
        anyhow::bail!(
            "bsdtar -tf failed: {}",
            String::from_utf8_lossy(&list.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&list.stdout);
    for line in stdout.lines() {
        for needle in forbidden_needles {
            if line == needle {
                return Ok(Some(format!(
                    "payload contains ABI-boundary path: {}",
                    needle.trim_start_matches("./")
                )));
            }
        }
    }

    Ok(None)
}

fn pack_erofs(source: &Path, dest: &Path) -> Result<()> {
    // mkfs.erofs -zlz4hc <dest> <source>
    let status = Command::new("mkfs.erofs")
        .arg(dest)
        .arg(source)
        .status()
        .context("Failed to run mkfs.erofs")?;

    if !status.success() {
        anyhow::bail!("mkfs.erofs failed");
    }
    Ok(())
}

fn ensure_dirs_writable(root: &Path) -> Result<()> {
    fn walk(dir: &Path) -> Result<()> {
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            let meta = std::fs::symlink_metadata(&path)
                .with_context(|| format!("symlink_metadata {}", path.display()))?;

            if meta.is_dir() {
                let mut perms = meta.permissions();
                let mode = perms.mode();
                // Ensure user r/w/x on directories (preserving all other bits).
                let new_mode = mode | 0o700;
                if new_mode != mode {
                    perms.set_mode(new_mode);
                    std::fs::set_permissions(&path, perms)
                        .with_context(|| format!("set_permissions {}", path.display()))?;
                }
                walk(&path)?;
            } else if meta.is_file() {
                // Ensure user-readable so mkfs.erofs can read file contents.
                let mut perms = meta.permissions();
                let mode = perms.mode();
                let new_mode = mode | 0o400;
                if new_mode != mode {
                    perms.set_mode(new_mode);
                    std::fs::set_permissions(&path, perms)
                        .with_context(|| format!("set_permissions {}", path.display()))?;
                }
            }
        }
        Ok(())
    }

    walk(root)
}

fn validate_staging_tree(root: &Path, allow_abi_boundary: bool) -> Result<()> {
    let forbidden_paths = [
        "lib64/ld-linux-x86-64.so.2",
        "usr/lib64/ld-linux-x86-64.so.2",
        "lib64/libc.so.6",
        "usr/lib64/libc.so.6",
        "lib64/libstdc++.so.6",
        "usr/lib64/libstdc++.so.6",
        "lib64/libgcc_s.so.1",
        "usr/lib64/libgcc_s.so.1",
    ];

    if !allow_abi_boundary {
        let mut found = Vec::new();
        for rel in forbidden_paths {
            let p = root.join(rel);
            if p.exists() {
                found.push(rel.to_string());
            }
        }
        if !found.is_empty() {
            anyhow::bail!(
                "deps overlay contains ABI-boundary files (poisoning risk): {}",
                found.join(", ")
            );
        }

        // Deps overlays should not ship or override base executables.
        // These directories are part of the "base runtime" surface area.
        for rel in ["bin", "sbin", "usr/bin", "usr/sbin"] {
            let p = root.join(rel);
            if p.exists() {
                anyhow::bail!(
                    "deps overlay contains '{}' (should be library-focused)",
                    rel
                );
            }
        }
    }

    // Wrong-arch scan: any non-x86_64 ELF in the overlay is a red flag *if it is
    // plausibly load-bearing for the runtime*.
    //
    // Some RPMs legitimately ship ELF objects that are not executed or dynamically
    // linked by the guest userspace (e.g. eBPF program objects, firmware-like blobs).
    // We ignore a small, explicit set of known non-load-bearing paths to keep the
    // invariant tight and explainable.
    let mut bad_elfs: Vec<String> = Vec::new();
    walk_files(root, &mut |path| {
        if is_non_load_bearing_elf_path(root, path) {
            return Ok(());
        }
        if let Some(machine) = elf_machine(path)? {
            // EM_X86_64 = 62
            if machine != 62 {
                bad_elfs.push(format!("{} (e_machine={})", path.display(), machine));
            }
        }
        Ok(())
    })?;

    if !bad_elfs.is_empty() {
        bad_elfs.sort();
        let sample = bad_elfs.iter().take(30).cloned().collect::<Vec<_>>();
        anyhow::bail!(
            "overlay contains non-x86_64 ELF files (sample): {}",
            sample.join("; ")
        );
    }

    Ok(())
}

fn is_non_load_bearing_elf_path(root: &Path, path: &Path) -> bool {
    // This should stay small and conservative. The goal is to ignore ELF artifacts
    // that are present in Fedora packages but not part of the guest userspace ABI.
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return false,
    };

    // eBPF program objects used by the host kernel; not dynamically linked into
    // userspace processes inside the guest.
    if rel.starts_with("usr/lib/bpf") || rel.starts_with("usr/lib64/bpf") {
        return true;
    }

    // BIOS blobs used by virtualization stacks; not runtime dependencies for
    // AppImages.
    if rel.starts_with("usr/share/seabios") {
        return true;
    }

    false
}

fn walk_files(root: &Path, f: &mut dyn FnMut(&Path) -> Result<()>) -> Result<()> {
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
    walk(root, f)
}

fn elf_machine(path: &Path) -> Result<Option<u16>> {
    // Minimal ELF header parse: if it's an ELF, read e_machine.
    // e_machine is at offset 18 (0x12), little-endian.
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut buf = [0u8; 64];
    use std::io::Read;
    let n = file
        .read(&mut buf)
        .with_context(|| format!("read {}", path.display()))?;
    if n < 20 {
        return Ok(None);
    }
    if &buf[0..4] != b"\x7FELF" {
        return Ok(None);
    }
    let machine = u16::from_le_bytes([buf[18], buf[19]]);
    Ok(Some(machine))
}

fn strip_gnu_property_notes(root: &Path) -> Result<usize> {
    let mut stripped = 0usize;
    walk_files(root, &mut |path| {
        if let Some(machine) = elf_machine(path)? {
            // EM_X86_64 = 62
            if machine == 62 {
                if elf_has_gnu_property_note(path)? {
                    let status = Command::new("objcopy")
                        .arg("--remove-section")
                        .arg(".note.gnu.property")
                        .arg(path)
                        .status()
                        .with_context(|| format!("Running objcopy on {}", path.display()))?;
                    if !status.success() {
                        anyhow::bail!("objcopy failed for {}", path.display());
                    }
                    stripped += 1;
                }
            }
        }
        Ok(())
    })?;

    Ok(stripped)
}

fn elf_has_gnu_property_note(path: &Path) -> Result<bool> {
    // Fast check for the existence of a .note.gnu.property section.
    //
    // We only implement what we need for typical 64-bit little-endian ELFs.
    // If parsing fails, fall back to "false" (do not strip) rather than risking
    // damaging unknown formats.
    use std::io::{Read, Seek, SeekFrom};

    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;

    let mut ehdr = [0u8; 64];
    if f.read(&mut ehdr)
        .with_context(|| format!("read {}", path.display()))?
        < 64
    {
        return Ok(false);
    }
    if &ehdr[0..4] != b"\x7FELF" {
        return Ok(false);
    }
    let class = ehdr[4];
    let data = ehdr[5];
    if class != 2 || data != 1 {
        // Not ELF64 little-endian
        return Ok(false);
    }

    // Offsets per ELF64 spec (little-endian)
    let e_shoff = u64::from_le_bytes(ehdr[40..48].try_into().unwrap());
    let e_shentsize = u16::from_le_bytes(ehdr[58..60].try_into().unwrap()) as u64;
    let e_shnum = u16::from_le_bytes(ehdr[60..62].try_into().unwrap()) as u64;
    let e_shstrndx = u16::from_le_bytes(ehdr[62..64].try_into().unwrap()) as u64;

    if e_shoff == 0 || e_shentsize == 0 || e_shnum == 0 || e_shstrndx >= e_shnum {
        return Ok(false);
    }

    // Read section header string table header
    let shstr_hdr_off = e_shoff + (e_shstrndx * e_shentsize);
    f.seek(SeekFrom::Start(shstr_hdr_off))
        .with_context(|| format!("seek shstrhdr {}", path.display()))?;

    let mut shdr = vec![0u8; e_shentsize as usize];
    f.read_exact(&mut shdr)
        .with_context(|| format!("read shstrhdr {}", path.display()))?;

    // ELF64_Shdr: sh_offset at 0x18, sh_size at 0x20
    if shdr.len() < 0x28 {
        return Ok(false);
    }
    let shstr_off = u64::from_le_bytes(shdr[0x18..0x20].try_into().unwrap());
    let shstr_size = u64::from_le_bytes(shdr[0x20..0x28].try_into().unwrap());
    if shstr_size == 0 {
        return Ok(false);
    }

    let shstr_size_usize = usize::try_from(shstr_size).unwrap_or(0);
    if shstr_size_usize == 0 || shstr_size_usize > 16 * 1024 * 1024 {
        // Avoid pathological allocations.
        return Ok(false);
    }

    let mut shstr = vec![0u8; shstr_size_usize];
    f.seek(SeekFrom::Start(shstr_off))
        .with_context(|| format!("seek shstr {}", path.display()))?;
    f.read_exact(&mut shstr)
        .with_context(|| format!("read shstr {}", path.display()))?;

    // Walk section headers; check section name against ".note.gnu.property".
    for i in 0..e_shnum {
        let off = e_shoff + (i * e_shentsize);
        f.seek(SeekFrom::Start(off))
            .with_context(|| format!("seek shdr {}", path.display()))?;
        let mut hdr = vec![0u8; e_shentsize as usize];
        f.read_exact(&mut hdr)
            .with_context(|| format!("read shdr {}", path.display()))?;
        if hdr.len() < 4 {
            continue;
        }
        let name_off = u32::from_le_bytes(hdr[0..4].try_into().unwrap()) as usize;
        if name_off >= shstr.len() {
            continue;
        }
        let name = &shstr[name_off..];
        let end = name.iter().position(|b| *b == 0).unwrap_or(0);
        if end == 0 {
            continue;
        }
        if &name[..end] == b".note.gnu.property" {
            return Ok(true);
        }
    }

    Ok(false)
}
