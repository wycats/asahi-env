use anyhow::Result;
use clap::{Parser, Subcommand};
use cmd_lib::run_fun;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Fedora release version
    #[arg(long, default_value = "41")]
    release: String,
}

#[derive(Subcommand)]
enum Commands {
    /// Find which package provides a file
    Provides { path: String },
    /// Get info about a package
    Info { package: String },
    /// Raw repoquery passthrough
    Query { args: Vec<String> },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let release = cli.release;
    let arch = "x86_64";

    match cli.command {
        Commands::Provides { path } => {
            println!(
                "Searching for provider of {} (Fedora {}, {})",
                path, release, arch
            );
            let output = run_fun!(
                dnf repoquery --releasever=$release --forcearch=$arch --whatprovides $path
            )?;
            println!("{}", output);
        }
        Commands::Info { package } => {
            let output = run_fun!(
                dnf info --releasever=$release --forcearch=$arch $package
            )?;
            println!("{}", output);
        }
        Commands::Query { args } => {
            let output = std::process::Command::new("dnf")
                .arg("repoquery")
                .arg(format!("--releasever={}", release))
                .arg(format!("--forcearch={}", arch))
                .args(args)
                .output()?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            print!("{}", stdout);
        }
    }

    Ok(())
}
