//! Minimal EXIF parser for overwriting the ASCII values of date-time tags
//! inside a JPEG's APP1 (Exif) segment directly in place, without changing
//! their length.
//!
//! # Crate selection notes
//!
//! - `kamadak-exif` is read-only and cannot write EXIF.
//! - Writing via `little_exif` or `img-parts` rebuilds the APP1 segment,
//!   which risks corrupting absolute offsets inside MakerNote and causing
//!   side effects on tags other than the date-times.
//! - An EXIF date-time is a fixed 19 bytes, "YYYY:MM:DD HH:MM:SS" (20 bytes
//!   including the NUL terminator), so its length never changes across a
//!   rewrite. Overwriting just the value area of the target tags therefore
//!   leaves every other part of the file — including the image scan data —
//!   completely untouched.
//!
//! For these reasons, instead of relying on external crates' write support,
//! we implement our own minimal TIFF/IFD walker that replaces the target
//! ASCII values inside APP1 in place. This lets tests guarantee that "the
//! differing bytes are confined to the target tags' value areas".

use crate::ExifDateTime;
use anyhow::{bail, ensure, Context, Result};

/// Length of an EXIF date-time "YYYY:MM:DD HH:MM:SS" (excluding the NUL terminator)
pub const DATETIME_LEN: usize = 19;

/// Absolute byte offsets (from the start of the file) of the date-time tag
/// values to be rewritten.
#[derive(Debug)]
pub struct DateTimeOffsets {
    /// DateTimeOriginal (0x9003, Exif IFD). Required.
    pub datetime_original: usize,
    /// DateTimeDigitized / CreateDate (0x9004, Exif IFD). Synced when present.
    pub datetime_digitized: Option<usize>,
    /// DateTime / ModifyDate (0x0132, IFD0). Synced when present.
    pub datetime: Option<usize>,
}

impl DateTimeOffsets {
    /// All offsets to be rewritten
    pub fn all(&self) -> Vec<usize> {
        let mut v = vec![self.datetime_original];
        v.extend(self.datetime_digitized);
        v.extend(self.datetime);
        v
    }
}

/// Finds the value offsets of the date-time tags in a JPEG buffer.
/// Missing DateTimeOriginal is an error. 0x9004 / 0x0132 are None when absent
/// (the in-place approach never creates tags that do not exist).
pub fn find_datetime_offsets(buf: &[u8]) -> Result<DateTimeOffsets> {
    let (tiff_base, seg_end) = find_exif_tiff(buf)?;
    let tiff = Tiff::new(buf, tiff_base, seg_end)?;

    let ifd0 = tiff.u32(4)? as usize;
    let mut datetime = None;
    let mut exif_ifd_off = None;
    for e in tiff.ifd_entries(ifd0)? {
        match e.tag {
            0x0132 => datetime = tiff.ascii_value_abs(&e).ok(),
            0x8769 => exif_ifd_off = Some(tiff.u32(e.value_field)? as usize),
            _ => {}
        }
    }

    let exif_ifd_off = exif_ifd_off.context("missing Exif IFD (tag 0x8769)")?;
    let mut datetime_original = None;
    let mut datetime_digitized = None;
    for e in tiff.ifd_entries(exif_ifd_off)? {
        match e.tag {
            0x9003 => datetime_original = Some(tiff.ascii_value_abs(&e)?),
            0x9004 => datetime_digitized = tiff.ascii_value_abs(&e).ok(),
            _ => {}
        }
    }

    Ok(DateTimeOffsets {
        datetime_original: datetime_original.context("missing DateTimeOriginal (tag 0x9003)")?,
        datetime_digitized,
        datetime,
    })
}

/// Reads the current value of DateTimeOriginal.
pub fn read_datetime_original(buf: &[u8]) -> Result<ExifDateTime> {
    let offsets = find_datetime_offsets(buf)?;
    read_datetime_at(buf, offsets.datetime_original)
}

/// Reads the 19-byte date-time at the given offset.
pub fn read_datetime_at(buf: &[u8], offset: usize) -> Result<ExifDateTime> {
    ExifDateTime::from_exif_bytes(&buf[offset..offset + DATETIME_LEN])
}

/// Overwrites the value areas (19 bytes each) of the found date-time tags
/// with the new date-time. ExifDateTime is guaranteed by its type to always
/// have a valid 19-byte representation, so no validation is needed and the
/// buffer length never changes.
pub fn patch_datetimes(buf: &mut [u8], offsets: &DateTimeOffsets, new: ExifDateTime) {
    let bytes = new.to_exif_bytes();
    for off in offsets.all() {
        buf[off..off + DATETIME_LEN].copy_from_slice(&bytes);
    }
}

/// Walks the JPEG segments and returns the APP1 (Exif) segment's
/// (absolute offset of the TIFF header, absolute offset of the segment end).
fn find_exif_tiff(buf: &[u8]) -> Result<(usize, usize)> {
    ensure!(
        buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8,
        "not a JPEG file (missing SOI marker)"
    );
    let mut pos = 2;
    while pos + 2 <= buf.len() {
        ensure!(
            buf[pos] == 0xFF,
            "invalid JPEG segment structure (offset {pos})"
        );
        let marker = buf[pos + 1];
        // Tolerate fill bytes (runs of 0xFF)
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        // EXIF never appears after SOS (start of scan data)
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        // Standalone markers without a length field (TEM, RSTn)
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }
        ensure!(pos + 4 <= buf.len(), "segment header is truncated");
        let len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        ensure!(
            len >= 2 && pos + 2 + len <= buf.len(),
            "segment length exceeds file size"
        );
        if marker == 0xE1 && len >= 2 + 6 + 8 && &buf[pos + 4..pos + 10] == b"Exif\0\0" {
            // pos+10 is the start of the TIFF header, pos+2+len the segment end
            return Ok((pos + 10, pos + 2 + len));
        }
        pos += 2 + len;
    }
    bail!("APP1 (Exif) segment not found")
}

/// Helper for reading the TIFF structure. All offsets are relative to the
/// TIFF header (base).
struct Tiff<'a> {
    buf: &'a [u8],
    base: usize,
    end: usize,
    little_endian: bool,
}

/// An IFD entry. `value_field` is the base-relative offset of the 4-byte
/// value/offset field.
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    value_field: usize,
}

impl<'a> Tiff<'a> {
    fn new(buf: &'a [u8], base: usize, end: usize) -> Result<Self> {
        ensure!(end <= buf.len() && end - base >= 8, "TIFF header too short");
        let little_endian = match &buf[base..base + 2] {
            b"II" => true,
            b"MM" => false,
            _ => bail!("invalid TIFF byte order"),
        };
        let tiff = Tiff {
            buf,
            base,
            end,
            little_endian,
        };
        ensure!(tiff.u16(2)? == 42, "invalid TIFF magic number");
        Ok(tiff)
    }

    fn bytes(&self, rel: usize, n: usize) -> Result<&[u8]> {
        let abs = self.base + rel;
        ensure!(
            abs + n <= self.end,
            "TIFF structure points outside the segment"
        );
        Ok(&self.buf[abs..abs + n])
    }

    fn u16(&self, rel: usize) -> Result<u16> {
        let b = self.bytes(rel, 2)?;
        Ok(if self.little_endian {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, rel: usize) -> Result<u32> {
        let b = self.bytes(rel, 4)?;
        Ok(if self.little_endian {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }

    fn ifd_entries(&self, ifd: usize) -> Result<Vec<Entry>> {
        let n = self.u16(ifd)? as usize;
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let e = ifd + 2 + i * 12;
            entries.push(Entry {
                tag: self.u16(e)?,
                typ: self.u16(e + 2)?,
                count: self.u32(e + 4)?,
                value_field: e + 8,
            });
        }
        Ok(entries)
    }

    /// Returns the absolute offset of an ASCII date-time tag's value area.
    /// A date-time is 19 chars + NUL = 20 bytes, which exceeds 4 bytes, so the
    /// value is always an offset reference.
    fn ascii_value_abs(&self, e: &Entry) -> Result<usize> {
        ensure!(
            e.typ == 2,
            "date-time tag 0x{:04X} is not ASCII type",
            e.tag
        );
        ensure!(
            e.count as usize >= DATETIME_LEN,
            "date-time tag 0x{:04X} has invalid length",
            e.tag
        );
        let off = self.u32(e.value_field)? as usize;
        let abs = self.base + off;
        ensure!(
            abs + DATETIME_LEN <= self.end,
            "date-time tag 0x{:04X} value points outside the segment",
            e.tag
        );
        Ok(abs)
    }
}
