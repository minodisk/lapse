use clap::Parser;
use lapse::Options;
use std::path::PathBuf;
use std::process::ExitCode;

/// 連写 JPEG の DateTimeOriginal をファイル名の自然順で秒単位に一意化し、
/// Google フォトで撮影順が保たれるようにするツール。
/// 同一秒に潰れた写真だけを最小限ずらし、時間の隙間がある別シーンの時刻は保つ。
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// 対象ディレクトリ（直下の .jpg / .jpeg のみ。サブディレクトリは再帰しない）
    dir: PathBuf,

    /// 実際には書き換えず、設定される予定の新しいタイムスタンプを一覧表示する
    #[arg(long)]
    dry_run: bool,

    /// 処理したファイルと変更前後の時刻を表示する
    #[arg(long)]
    verbose: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let summary = match lapse::run(&args.dir, &Options { dry_run: args.dry_run }) {
        Ok(summary) => summary,
        Err(e) => {
            eprintln!("エラー: {e:#}");
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
        println!("{succeeded} 件のファイルを書き換え予定");
    } else {
        println!("{succeeded} 件のファイルを書き換えました");
    }

    if !failures.is_empty() {
        eprintln!("{} 件のファイルで失敗:", failures.len());
        for (path, result) in &failures {
            if let Err(e) = result {
                eprintln!("  {}: {e:#}", path.display());
            }
        }
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
