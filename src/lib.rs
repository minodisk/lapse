//! Google フォトが連写写真のサブ秒タイムスタンプを無視して同一秒内の順序を
//! 保持しない問題を回避するため、ディレクトリ直下の JPEG の DateTimeOriginal を
//! ファイル名の自然順に 1 秒ずつ進めて一意化するライブラリ。

pub mod exif_patch;

use anyhow::{bail, Context, Result};
use chrono::{Duration, NaiveDateTime};
use rayon::prelude::*;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// EXIF 日時フォーマット "YYYY:MM:DD HH:MM:SS"（コロン区切り・固定 19 バイト）
pub const EXIF_DATETIME_FORMAT: &str = "%Y:%m:%d %H:%M:%S";

pub struct Options {
    /// true なら書き換えを行わず、割り当て予定の日時だけを計算する
    pub dry_run: bool,
}

/// 1 ファイルの処理結果（変更前後の DateTimeOriginal）
#[derive(Debug)]
pub struct FileOutcome {
    pub old: String,
    pub new: String,
}

/// 全ファイルの処理結果。処理順（自然順ソート順）を保持する。
#[derive(Debug)]
pub struct Summary {
    pub results: Vec<(PathBuf, Result<FileOutcome>)>,
}

/// 指定ディレクトリ直下の JPEG (.jpg / .jpeg、大文字小文字不問) を
/// ファイル名の自然順ソートで列挙する。RAW 等の他形式は無視する。
/// サブディレクトリは再帰しない（将来 --recursive を足すならここを拡張する）。
pub fn collect_jpegs(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(dir)
        .with_context(|| format!("ディレクトリを読めません: {}", dir.display()))?;
    let mut named: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let is_jpeg = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg"))
            .unwrap_or(false);
        if !is_jpeg {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        named.push((name, path));
    }
    // 自然順ソート: image_2.jpg が image_10.jpg より前に来るよう、
    // 数値部分を数値として比較する (natord)。単純な辞書順にはしない。
    named.sort_by(|a, b| natord::compare(&a.0, &b.0));
    Ok(named.into_iter().map(|(_, p)| p).collect())
}

/// ディレクトリ内の JPEG を処理する。
///
/// 1. 自然順ソートで対象を確定し、先頭ファイルの DateTimeOriginal を基準時刻とする
/// 2. インデックス i のファイルに「基準時刻 + i 秒」を割り当てる（ここまで逐次）
/// 3. 各ファイルの読み込み・書き換え・保存を並列実行する
///    （割り当て秒は手順 2 で確定済みなので、並列化で順序がずれることはない）
pub fn run(dir: &Path, opts: &Options) -> Result<Summary> {
    let files = collect_jpegs(dir)?;
    if files.is_empty() {
        bail!("対象の JPEG ファイルが見つかりません: {}", dir.display());
    }

    let first = &files[0];
    let first_buf = fs::read(first)
        .with_context(|| format!("読み込みに失敗: {}", first.display()))?;
    let base_str = exif_patch::read_datetime_original(&first_buf).with_context(|| {
        format!(
            "先頭ファイル {} から基準時刻 (DateTimeOriginal) を取得できません",
            first.display()
        )
    })?;
    let base = NaiveDateTime::parse_from_str(base_str.trim(), EXIF_DATETIME_FORMAT)
        .with_context(|| {
            format!(
                "先頭ファイル {} の DateTimeOriginal を日時として解釈できません: {base_str:?}",
                first.display()
            )
        })?;

    let jobs: Vec<(PathBuf, String)> = files
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            let new = (base + Duration::seconds(i as i64))
                .format(EXIF_DATETIME_FORMAT)
                .to_string();
            (path, new)
        })
        .collect();

    // 1 ファイルの失敗で全体を止めず、ファイル単位で Result を収集する
    let results: Vec<(PathBuf, Result<FileOutcome>)> = jobs
        .into_par_iter()
        .map(|(path, new)| {
            let result = process_file(&path, &new, opts.dry_run);
            (path, result)
        })
        .collect();

    Ok(Summary { results })
}

fn process_file(path: &Path, new: &str, dry_run: bool) -> Result<FileOutcome> {
    let mut buf = fs::read(path).with_context(|| format!("読み込みに失敗: {}", path.display()))?;
    let offsets = exif_patch::find_datetime_offsets(&buf)?;
    let old = exif_patch::read_at(&buf, offsets.datetime_original)?;
    exif_patch::patch_datetimes(&mut buf, &offsets, new)?;
    if !dry_run {
        write_atomic(path, &buf)?;
    }
    Ok(FileOutcome {
        old,
        new: new.to_string(),
    })
}

/// 書き込み途中でクラッシュしても元ファイルが壊れないよう、
/// 同一ディレクトリのテンポラリファイルに書き切ってから rename で置き換える。
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("親ディレクトリを特定できません: {}", path.display()))?;
    let permissions = fs::metadata(path)?.permissions();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("上書き保存に失敗: {}", path.display()))?;
    fs::set_permissions(path, permissions)?;
    Ok(())
}
