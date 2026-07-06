//! Library that uniquifies the DateTimeOriginal of JPEGs directly under a
//! directory at second granularity in natural filename order, to work around
//! Google Photos ignoring sub-second timestamps of burst photos and not
//! preserving the order within the same second.
//!
//! Assignment rule: new time = max(original time, previous file's new time + 1 second).
//! Only burst shots collapsed into the same second are shifted minimally; if
//! there is a time gap before another scene, that scene's original capture
//! time is preserved as is.

mod exif_datetime;
pub mod exif_patch;

pub use exif_datetime::{ExifDateTime, EXIF_DATETIME_FORMAT};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Options {
    /// If true, do not rewrite; only compute the date-times to be assigned
    pub dry_run: bool,
}

/// Result of processing one file (DateTimeOriginal before/after)
#[derive(Debug)]
pub struct FileOutcome {
    pub old: ExifDateTime,
    pub new: ExifDateTime,
}

/// Results for all files, kept in processing order (natural sort order).
#[derive(Debug)]
pub struct Summary {
    pub results: Vec<(PathBuf, Result<FileOutcome>)>,
}

/// Enumerates the JPEGs (.jpg / .jpeg, case-insensitive) directly under the
/// given directory in natural filename order. Other formats such as RAW are
/// ignored. Subdirectories are not recursed (extend here if --recursive is
/// ever added).
pub fn collect_jpegs(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(dir)
        .with_context(|| format!("failed to read directory: {}", dir.display()))?;
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
    // Natural sort: compare numeric parts as numbers (natord) so that
    // image_2.jpg comes before image_10.jpg. Not plain lexicographic order.
    named.sort_by(|a, b| natord::compare(&a.0, &b.0));
    Ok(named.into_iter().map(|(_, p)| p).collect())
}

/// Processes the JPEGs in a directory.
///
/// 1. Fix the targets by natural sort, then read each file's original
///    DateTimeOriginal in parallel
/// 2. In sort order, sequentially finalize "new time = max(original time,
///    previous new time + 1 second)" (the first file keeps its original time;
///    only files collapsed into the same second are pushed forward, and
///    original times of scenes with time gaps are preserved)
/// 3. Rewrite and save each file in parallel (the assigned times are already
///    fixed in step 2, so parallelism cannot reorder them)
pub fn run(dir: &Path, opts: &Options) -> Result<Summary> {
    let files = collect_jpegs(dir)?;
    if files.is_empty() {
        bail!("no target JPEG files found in: {}", dir.display());
    }

    let originals: Vec<(PathBuf, Result<ExifDateTime>)> = files
        .into_par_iter()
        .map(|path| {
            let original = read_original_datetime(&path);
            (path, original)
        })
        .collect();

    // Files whose original time cannot be read are excluded from assignment
    // and collected as errors
    let mut prev: Option<ExifDateTime> = None;
    let jobs: Vec<(PathBuf, Result<ExifDateTime>)> = originals
        .into_iter()
        .map(|(path, original)| {
            let assigned = original.map(|t| {
                let new = match prev {
                    Some(p) => t.max(p.succ()),
                    None => t,
                };
                prev = Some(new);
                new
            });
            (path, assigned)
        })
        .collect();

    // One file's failure does not stop the whole run; collect a Result per file
    let results: Vec<(PathBuf, Result<FileOutcome>)> = jobs
        .into_par_iter()
        .map(|(path, assigned)| {
            let result = assigned.and_then(|new| process_file(&path, new, opts.dry_run));
            (path, result)
        })
        .collect();

    Ok(Summary { results })
}

fn read_original_datetime(path: &Path) -> Result<ExifDateTime> {
    let buf = fs::read(path).with_context(|| format!("failed to read: {}", path.display()))?;
    exif_patch::read_datetime_original(&buf)
}

fn process_file(path: &Path, new: ExifDateTime, dry_run: bool) -> Result<FileOutcome> {
    let mut buf = fs::read(path).with_context(|| format!("failed to read: {}", path.display()))?;
    let offsets = exif_patch::find_datetime_offsets(&buf)?;
    let old = exif_patch::read_datetime_at(&buf, offsets.datetime_original)?;
    // Skip writing entirely if every target tag already matches the assigned time
    let new_bytes = new.to_exif_bytes();
    let unchanged = offsets
        .all()
        .into_iter()
        .all(|o| buf[o..o + exif_patch::DATETIME_LEN] == new_bytes);
    if !unchanged {
        exif_patch::patch_datetimes(&mut buf, &offsets, new);
        if !dry_run {
            write_atomic(path, &buf)?;
        }
    }
    Ok(FileOutcome { old, new })
}

/// Writes the whole file to a temporary file in the same directory and then
/// replaces the original via rename, so a crash mid-write never corrupts the
/// original file.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .with_context(|| format!("cannot determine parent directory of: {}", path.display()))?;
    let permissions = fs::metadata(path)?.permissions();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("failed to overwrite: {}", path.display()))?;
    fs::set_permissions(path, permissions)?;
    Ok(())
}
