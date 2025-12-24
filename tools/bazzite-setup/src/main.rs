use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

mod ops;

#[derive(Parser, Debug)]
#[command(name = "bazzite-setup")]
#[command(
    about = "Idempotent setup/verification tooling for Bazzite hosts (portable workstation defaults)",
    long_about = None
)]
struct Cli {
    /// By default, commands will use sudo for operations that require it.
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

        /// Alias for `target=all`.
        #[arg(long)]
        all: bool,
    },

    /// Apply changes. Some operations require privileges and/or reboot (rpm-ostree).
    Apply {
        #[arg(value_enum, default_value_t = Target::All)]
        target: Target,

        /// Alias for `target=all`.
        #[arg(long)]
        all: bool,

        /// Do not write; print planned actions.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Target {
    /// Install/configure keyd and the default keyd layer.
    Keyd,

    /// Install/apply visual themes (Papirus + Bibata + GTK3 adw theme).
    Themes,

    /// Apply GNOME defaults from the runbook (touchpad/battery/etc).
    GnomeDefaults,

    /// All supported operations.
    All,
}

fn main() -> Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();
    let allow_sudo = !cli.no_sudo;

    match cli.command {
        Command::Check { mut target, all } => {
            if all {
                target = Target::All;
            }

            match target {
                Target::Keyd => ops::keyd::check(allow_sudo).context("keyd check")?,
                Target::Themes => ops::themes::check(allow_sudo).context("themes check")?,
                Target::GnomeDefaults => {
                    ops::gnome_defaults::check(allow_sudo).context("gnome-defaults check")?
                }
                Target::All => {
                    ops::keyd::check(allow_sudo).context("keyd check")?;
                    ops::themes::check(allow_sudo).context("themes check")?;
                    ops::gnome_defaults::check(allow_sudo).context("gnome-defaults check")?;
                }
            }
        }

        Command::Apply {
            mut target,
            all,
            dry_run,
        } => {
            if all {
                target = Target::All;
            }

            match target {
                Target::Keyd => ops::keyd::apply(allow_sudo, dry_run).context("keyd apply")?,
                Target::Themes => {
                    ops::themes::apply(allow_sudo, dry_run).context("themes apply")?
                }
                Target::GnomeDefaults => ops::gnome_defaults::apply(allow_sudo, dry_run)
                    .context("gnome-defaults apply")?,
                Target::All => {
                    ops::keyd::apply(allow_sudo, dry_run).context("keyd apply")?;
                    ops::themes::apply(allow_sudo, dry_run).context("themes apply")?;
                    ops::gnome_defaults::apply(allow_sudo, dry_run)
                        .context("gnome-defaults apply")?;
                }
            }
        }
    }

    Ok(())
}
