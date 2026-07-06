//! EXIF 日時 "YYYY:MM:DD HH:MM:SS" を表すニュータイプ。
//!
//! この型の値は「有効な EXIF 日時である」ことを構築時のパースで保証する
//! (Parse, don't validate)。検証は境界（ファイルからの読み取り）の 1 箇所に
//! 集約され、以降の計算・書き込みでは実行時検証が不要になる。

use anyhow::{Context, Error, Result};
use chrono::{Duration, NaiveDateTime};
use std::fmt;
use std::str::FromStr;

/// EXIF の日時フォーマット（コロン区切り・固定 19 バイト）
pub const EXIF_DATETIME_FORMAT: &str = "%Y:%m:%d %H:%M:%S";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExifDateTime(NaiveDateTime);

impl ExifDateTime {
    /// 1 秒後の日時。分・時・日の繰り上がりは chrono が処理する。
    pub fn succ(self) -> Self {
        Self(self.0 + Duration::seconds(1))
    }

    /// EXIF タグに書き込む固定 19 バイト表現。
    pub fn to_exif_bytes(self) -> [u8; 19] {
        let s = self.0.format(EXIF_DATETIME_FORMAT).to_string();
        // 西暦 5 桁以上は EXIF で表現できず、パース由来の値からは到達しない
        s.into_bytes()
            .try_into()
            .expect("EXIF 日時は 19 バイト固定")
    }

    /// EXIF タグの値領域のバイト列からパースする。
    pub fn from_exif_bytes(bytes: &[u8]) -> Result<Self> {
        let s = std::str::from_utf8(bytes).context("日時タグの値が ASCII ではありません")?;
        s.parse()
    }
}

impl FromStr for ExifDateTime {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        NaiveDateTime::parse_from_str(s.trim(), EXIF_DATETIME_FORMAT)
            .map(Self)
            .with_context(|| format!("EXIF 日時 (YYYY:MM:DD HH:MM:SS) として解釈できません: {s:?}"))
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
