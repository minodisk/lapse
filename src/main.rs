use clap::Parser;
use lapse::Options;
use std::path::PathBuf;
use std::process::ExitCode;

/// Uniquifies the DateTimeOriginal of burst JPEGs at second granularity in
/// natural filename order so that Google Photos keeps the shooting order.
/// Only photos collapsed into the same second are shifted minimally; times of
/// separate scenes with time gaps are preserved.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Target directory (only .jpg / .jpeg directly under it; subdirectories are not recursed)
    dir: PathBuf,

    /// Do not rewrite anything; list the new timestamps that would be set
    #[arg(long)]
    dry_run: bool,

    /// Print each processed file with its before/after times
    #[arg(long)]
    verbose: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let summary = match lapse::run(
        &args.dir,
        &Options {
            dry_run: args.dry_run,
        },
    ) {
        Ok(summary) => summary,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    let mut succeeded = 0usize;
    let mut failures = Vec::new();
    for (path, result) in &summary.results {
        match result {
            Ok(outcome) => {
                succeeded += 1;
                if args.dry_run {
                    println!(
                        "[dry-run] {}: {} -> {}",
                        path.display(),
                        outcome.old,
                        outcome.new
                    );
                } else if args.verbose {
                    println!("{}: {} -> {}", path.display(), outcome.old, outcome.new);
                }
            }
            Err(_) => failures.push((path, result)),
        }
    }

    if args.dry_run {
        println!("{succeeded} file(s) would be rewritten");
    } else {
        println!("rewrote {succeeded} file(s)");
    }

    if !failures.is_empty() {
        eprintln!("{} file(s) failed:", failures.len());
        for (path, result) in &failures {
            if let Err(e) = result {
                eprintln!("  {}: {e:#}", path.display());
            }
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
