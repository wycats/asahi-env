use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod ops;

#[derive(Parser, Debug)]
#[command(name = "asahi-setup")]
#[command(about = "Idempotent setup/verification tooling for the Mac-like Asahi workstation", long_about = None)]
struct Cli {
    /// By default, commands will use sudo for probes/operations that require it.
    ///
    /// Use this flag to force unprivileged behavior.
    #[arg(long, global = true)]
    no_sudo: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print what would change (no writes).
    Check {
        #[arg(value_enum, default_value_t = Target::All)]
        target: Target,
    },

    /// Apply changes. Requires sufficient permissions (often run via sudo).
    Apply {
        #[arg(value_enum, default_value_t = Target::All)]
        target: Target,

        /// Do not write; print planned actions.
        #[arg(long)]
        dry_run: bool,
    },

    /// Capture a baseline system snapshot (“doctor report”) for empirical debugging.
    Doctor {
        /// Write the JSON report to this path (in addition to printing output).
        #[arg(long)]
        output: Option<PathBuf>,

        /// Save the JSON report to the default snapshot directory.
        ///
        /// Defaults to `$XDG_STATE_HOME/asahi/doctor/` or `~/.local/state/asahi/doctor/`.
        #[arg(long)]
        save: bool,

        /// Emit JSON to stdout instead of human-readable text.
        #[arg(long)]
        json: bool,
    },

    /// Compare two doctor report JSON snapshots.
    DoctorDiff {
        /// Older snapshot JSON path.
        older: PathBuf,

        /// Newer snapshot JSON path.
        newer: PathBuf,

        /// Emit JSON diff to stdout.
        #[arg(long)]
        json: bool,
    },

    /// Render an existing doctor report JSON snapshot.
    DoctorShow {
        /// Snapshot JSON path.
        snapshot: PathBuf,

        /// Emit JSON to stdout instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Target {
    /// keyd + GNOME bindings
    Spotlight,

    /// titdb systemd service (touchpad deadzones)
    Titdb,

    /// All supported operations
    All,
}

fn main() -> Result<()> {
    #[cfg(unix)]
    unsafe {
        // Rust ignores SIGPIPE by default, which can turn common patterns like `... | head`
        // into a noisy panic. Restoring the default makes the process exit quietly.
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();
    let allow_sudo = !cli.no_sudo;

    match cli.command {
        Command::Check { target } => match target {
            Target::Spotlight => ops::spotlight::check(allow_sudo).context("spotlight check")?,
            Target::Titdb => ops::titdb::check(allow_sudo).context("titdb check")?,
            Target::All => {
                ops::spotlight::check(allow_sudo).context("spotlight check")?;
                ops::titdb::check(allow_sudo).context("titdb check")?;
            }
        },

        Command::Apply { target, dry_run } => match target {
            Target::Spotlight => {
                ops::spotlight::apply(allow_sudo, dry_run).context("spotlight apply")?
            }
            Target::Titdb => ops::titdb::apply(allow_sudo, dry_run).context("titdb apply")?,
            Target::All => {
                ops::spotlight::apply(allow_sudo, dry_run).context("spotlight apply")?;
                ops::titdb::apply(allow_sudo, dry_run).context("titdb apply")?;
            }
        },

        Command::Doctor { output, save, json } => {
            ops::doctor::run(allow_sudo, output, save, json).context("doctor report")?
        }

        Command::DoctorDiff { older, newer, json } => {
            ops::doctor::diff(older, newer, json).context("doctor diff")?
        }

        Command::DoctorShow { snapshot, json } => {
            ops::doctor::show(snapshot, json).context("doctor show")?
        }
    }

    Ok(())
}
