use anyhow::{Context, Result};
use clap::Parser;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to the AppImage
    #[arg(long)]
    appimage: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("Inspecting AppImage: {}", cli.appimage.display());

    // 1. Extract AppImage (using --appimage-extract)
    // Note: This assumes the AppImage supports this flag, which most do.
    // Alternatively, we could use our own extraction logic from appimage-runner.
    let extract_dir = PathBuf::from("squashfs-root");
    if extract_dir.exists() {
        std::fs::remove_dir_all(&extract_dir)?;
    }

    println!("Extracting...");
    let status = Command::new(&cli.appimage)
        .arg("--appimage-extract")
        .status()
        .context("Failed to run AppImage with --appimage-extract")?;

    if !status.success() {
        anyhow::bail!("AppImage extraction failed");
    }

    // 2. Scan for ELF files
    println!("Scanning for ELF files...");
    let mut needed_libs = HashSet::new();

    for entry in walkdir::WalkDir::new(&extract_dir) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();

        // Check if ELF
        if let Ok(mut file) = std::fs::File::open(path) {
            use std::io::Read;
            let mut magic = [0u8; 4];
            if file.read_exact(&mut magic).is_ok() && magic == [0x7f, b'E', b'L', b'F'] {
                // It's an ELF. Read DT_NEEDED.
                if let Ok(libs) = get_needed_libs(path) {
                    for lib in libs {
                        needed_libs.insert(lib);
                    }
                }
            }
        }
    }

    println!(
        "Found {} unique shared library dependencies.",
        needed_libs.len()
    );

    // 3. Filter out libs provided by the AppImage itself
    // (This is a simplification; real logic needs to check RPATH/LD_LIBRARY_PATH)

    // 4. Print missing libs (candidates for the base image)
    println!("\nPotential System Dependencies:");
    let mut sorted_libs: Vec<_> = needed_libs.into_iter().collect();
    sorted_libs.sort();

    for lib in sorted_libs {
        println!("- {}", lib);
    }

    // Cleanup
    std::fs::remove_dir_all(&extract_dir)?;

    Ok(())
}

fn get_needed_libs(path: &std::path::Path) -> Result<Vec<String>> {
    // Use 'readelf' or 'objdump' if available, or a Rust ELF parser.
    // For simplicity in this prototype, we'll use the 'elf' crate if we added it,
    // or just shell out to readelf.

    let output = Command::new("readelf").arg("-d").arg(path).output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut libs = Vec::new();

    for line in stdout.lines() {
        if line.contains("(NEEDED)") {
            // Format: 0x0000000000000001 (NEEDED)             Shared library: [libname.so]
            if let Some(start) = line.find('[') {
                if let Some(end) = line.find(']') {
                    libs.push(line[start + 1..end].to_string());
                }
            }
        }
    }

    Ok(libs)
}
