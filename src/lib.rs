//! Google フォトが連写写真のサブ秒タイムスタンプを無視して同一秒内の順序を
//! 保持しない問題を回避するため、ディレクトリ直下の JPEG の DateTimeOriginal を
//! ファイル名の自然順で秒単位に一意化するライブラリ。
//!
//! 割り当てルール: 新時刻 = max(元の時刻, 直前のファイルの新時刻 + 1 秒)。
//! 同一秒に潰れた連写だけが最小限ずらされ、別シーンとの間に時間の隙間が
//! あればそのシーンの元の撮影時刻はそのまま保たれる。

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
/// 1. 自然順ソートで対象を確定し、各ファイルの元の DateTimeOriginal を並列で読む
/// 2. ソート順に「新時刻 = max(元の時刻, 直前の新時刻 + 1 秒)」を逐次確定する
///    （先頭は元の時刻のまま。同一秒に潰れたファイルだけが押し出され、
///    時間の隙間があるシーンの元時刻は保たれる）
/// 3. 各ファイルの書き換え・保存を並列実行する
///    （割り当て時刻は手順 2 で確定済みなので、並列化で順序がずれることはない）
pub fn run(dir: &Path, opts: &Options) -> Result<Summary> {
    let files = collect_jpegs(dir)?;
    if files.is_empty() {
        bail!("対象の JPEG ファイルが見つかりません: {}", dir.display());
    }

    let originals: Vec<(PathBuf, Result<NaiveDateTime>)> = files
        .into_par_iter()
        .map(|path| {
            let original = read_original_datetime(&path);
            (path, original)
        })
        .collect();

    // 元時刻を読めなかったファイルは割り当てから除外し、エラーとして収集する
    let mut prev: Option<NaiveDateTime> = None;
    let jobs: Vec<(PathBuf, Result<String>)> = originals
        .into_iter()
        .map(|(path, original)| {
            let assigned = original.map(|t| {
                let new = match prev {
                    Some(p) => t.max(p + Duration::seconds(1)),
                    None => t,
                };
                prev = Some(new);
                new.format(EXIF_DATETIME_FORMAT).to_string()
            });
            (path, assigned)
        })
        .collect();

    // 1 ファイルの失敗で全体を止めず、ファイル単位で Result を収集する
    let results: Vec<(PathBuf, Result<FileOutcome>)> = jobs
        .into_par_iter()
        .map(|(path, assigned)| {
            let result = assigned.and_then(|new| process_file(&path, &new, opts.dry_run));
            (path, result)
        })
        .collect();

    Ok(Summary { results })
}

fn read_original_datetime(path: &Path) -> Result<NaiveDateTime> {
    let buf = fs::read(path).with_context(|| format!("読み込みに失敗: {}", path.display()))?;
    let s = exif_patch::read_datetime_original(&buf)?;
    NaiveDateTime::parse_from_str(s.trim(), EXIF_DATETIME_FORMAT)
        .with_context(|| format!("DateTimeOriginal を日時として解釈できません: {s:?}"))
}

fn process_file(path: &Path, new: &str, dry_run: bool) -> Result<FileOutcome> {
    let mut buf = fs::read(path).with_context(|| format!("読み込みに失敗: {}", path.display()))?;
    let offsets = exif_patch::find_datetime_offsets(&buf)?;
    let old = exif_patch::read_at(&buf, offsets.datetime_original)?;
    // 全対象タグが既に割り当て時刻と一致していれば書き込み自体をスキップする
    let unchanged = offsets
        .all()
        .into_iter()
        .all(|o| &buf[o..o + exif_patch::DATETIME_LEN] == new.as_bytes());
    if !unchanged {
        exif_patch::patch_datetimes(&mut buf, &offsets, new)?;
        if !dry_run {
            write_atomic(path, &buf)?;
        }
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
