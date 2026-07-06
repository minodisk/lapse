//! Newtype representing an EXIF date-time "YYYY:MM:DD HH:MM:SS".
//!
//! A value of this type is guaranteed to be a valid EXIF date-time by parsing
//! at construction (Parse, don't validate). Validation is consolidated at the
//! single boundary (reading from a file), so no runtime validation is needed
//! in later computation and writing.

use anyhow::{Context, Error, Result};
use chrono::{Duration, NaiveDateTime};
use std::fmt;
use std::str::FromStr;

/// EXIF date-time format (colon-separated, fixed 19 bytes)
pub const EXIF_DATETIME_FORMAT: &str = "%Y:%m:%d %H:%M:%S";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExifDateTime(NaiveDateTime);

impl ExifDateTime {
    /// The date-time one second later. Minute/hour/day carry-over is handled
    /// by chrono.
    pub fn succ(self) -> Self {
        Self(self.0 + Duration::seconds(1))
    }

    /// The fixed 19-byte representation written into EXIF tags.
    pub fn to_exif_bytes(self) -> [u8; 19] {
        let s = self.0.format(EXIF_DATETIME_FORMAT).to_string();
        // Years with 5+ digits cannot be represented in EXIF and are
        // unreachable from parsed values
        s.into_bytes()
            .try_into()
            .expect("EXIF date-time is always 19 bytes")
    }

    /// Parses from the bytes of an EXIF tag value area.
    pub fn from_exif_bytes(bytes: &[u8]) -> Result<Self> {
        let s = std::str::from_utf8(bytes).context("date-time tag value is not ASCII")?;
        s.parse()
    }
}

impl FromStr for ExifDateTime {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        NaiveDateTime::parse_from_str(s.trim(), EXIF_DATETIME_FORMAT)
            .map(Self)
            .with_context(|| format!("cannot parse as EXIF date-time (YYYY:MM:DD HH:MM:SS): {s:?}"))
    }
}

impl fmt::Display for ExifDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.format(EXIF_DATETIME_FORMAT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display_roundtrip() {
        let dt: ExifDateTime = "2024:01:02 03:04:59".parse().unwrap();
        assert_eq!(dt.to_string(), "2024:01:02 03:04:59");
        assert_eq!(&dt.to_exif_bytes(), b"2024:01:02 03:04:59");
    }

    #[test]
    fn parse_rejects_invalid_format() {
        assert!("2024-01-02 03:04:59".parse::<ExifDateTime>().is_err());
        assert!("not a datetime".parse::<ExifDateTime>().is_err());
        assert!("".parse::<ExifDateTime>().is_err());
    }

    #[test]
    fn succ_rolls_over_minute() {
        let dt: ExifDateTime = "2024:12:31 23:59:59".parse().unwrap();
        assert_eq!(dt.succ().to_string(), "2025:01:01 00:00:00");
    }

    #[test]
    fn ord_and_max() {
        let a: ExifDateTime = "2024:01:02 03:04:59".parse().unwrap();
        let b: ExifDateTime = "2024:01:02 03:05:10".parse().unwrap();
        assert_eq!(a.max(b), b);
        assert_eq!(b.max(a.succ()), b);
    }
}
