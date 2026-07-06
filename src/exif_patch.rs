//! JPEG の APP1(Exif) セグメント内にある日時タグの ASCII 値を、
//! 長さを変えずにインプレースで直接上書きするための最小 EXIF パーサ。
//!
//! # crate 選定メモ
//!
//! - `kamadak-exif` は EXIF の読み取り専用で書き込みができない。
//! - `little_exif` や `img-parts` を使う書き込みは APP1 セグメントの再構築を伴い、
//!   MakerNote 内の絶対オフセット破損や、日時以外のタグへの副作用のリスクがある。
//! - EXIF の日時は "YYYY:MM:DD HH:MM:SS" の固定 19 バイト（NUL 終端込みで 20 バイト）で、
//!   書き換え前後で長さが変わらない。そのため該当タグの値領域だけをバイト単位で
//!   上書きすれば、画像スキャンデータを含むファイルの他の部分に一切触れずに済む。
//!
//! 以上から、外部 crate の書き込み機能には頼らず、APP1 内の該当 ASCII 値を
//! インプレース置換する自前の最小 TIFF/IFD ウォーカーを実装している。
//! これにより「差分バイトが対象タグの値領域のみ」であることをテストで保証できる。

use anyhow::{bail, ensure, Context, Result};

/// EXIF 日時 "YYYY:MM:DD HH:MM:SS" の長さ（NUL 終端を含まない）
pub const DATETIME_LEN: usize = 19;

/// 書き換え対象の日時タグ値の、ファイル先頭からの絶対バイトオフセット。
#[derive(Debug)]
pub struct DateTimeOffsets {
    /// DateTimeOriginal (0x9003, Exif IFD)。必須。
    pub datetime_original: usize,
    /// DateTimeDigitized / CreateDate (0x9004, Exif IFD)。存在すれば揃える。
    pub datetime_digitized: Option<usize>,
    /// DateTime / ModifyDate (0x0132, IFD0)。存在すれば揃える。
    pub datetime: Option<usize>,
}

impl DateTimeOffsets {
    /// 書き換え対象となる全オフセット
    pub fn all(&self) -> Vec<usize> {
        let mut v = vec![self.datetime_original];
        v.extend(self.datetime_digitized);
        v.extend(self.datetime);
        v
    }
}

/// JPEG バッファから日時タグの値オフセットを探す。
/// DateTimeOriginal が無い場合はエラー。0x9004 / 0x0132 は無ければ None
/// （インプレース方式では存在しないタグを新規作成しない）。
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

    let exif_ifd_off = exif_ifd_off.context("Exif IFD (タグ 0x8769) がありません")?;
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
        datetime_original: datetime_original.context("DateTimeOriginal (タグ 0x9003) がありません")?,
        datetime_digitized,
        datetime,
    })
}

/// DateTimeOriginal の現在値を文字列で読み取る。
pub fn read_datetime_original(buf: &[u8]) -> Result<String> {
    let offsets = find_datetime_offsets(buf)?;
    read_at(buf, offsets.datetime_original)
}

/// 指定オフセットにある 19 バイトの日時文字列を読み取る。
pub fn read_at(buf: &[u8], offset: usize) -> Result<String> {
    let bytes = &buf[offset..offset + DATETIME_LEN];
    let s = std::str::from_utf8(bytes).context("日時タグの値が ASCII ではありません")?;
    Ok(s.to_string())
}

/// 見つかった日時タグの値領域（各 19 バイト）を新しい日時文字列で上書きする。
/// バッファ長は変化しない。
pub fn patch_datetimes(buf: &mut [u8], offsets: &DateTimeOffsets, new: &str) -> Result<()> {
    ensure!(
        new.len() == DATETIME_LEN,
        "日時文字列は {DATETIME_LEN} バイト固定です: {new:?}"
    );
    for off in offsets.all() {
        buf[off..off + DATETIME_LEN].copy_from_slice(new.as_bytes());
    }
    Ok(())
}

/// JPEG セグメントを走査し、APP1(Exif) の
/// (TIFF ヘッダの絶対オフセット, セグメント末尾の絶対オフセット) を返す。
fn find_exif_tiff(buf: &[u8]) -> Result<(usize, usize)> {
    ensure!(
        buf.len() >= 2 && buf[0] == 0xFF && buf[1] == 0xD8,
        "JPEG ファイルではありません (SOI マーカーがない)"
    );
    let mut pos = 2;
    while pos + 2 <= buf.len() {
        ensure!(buf[pos] == 0xFF, "不正な JPEG セグメント構造 (offset {pos})");
        let marker = buf[pos + 1];
        // fill byte (0xFF の連続) を許容
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        // SOS (スキャンデータ開始) 以降に EXIF は現れない
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        // 長さフィールドを持たないスタンドアロンマーカー (TEM, RSTn)
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            pos += 2;
            continue;
        }
        ensure!(pos + 4 <= buf.len(), "セグメントヘッダが途中で切れています");
        let len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        ensure!(
            len >= 2 && pos + 2 + len <= buf.len(),
            "セグメント長がファイルサイズを超えています"
        );
        if marker == 0xE1 && len >= 2 + 6 + 8 && &buf[pos + 4..pos + 10] == b"Exif\0\0" {
            // pos+10 が TIFF ヘッダの先頭、pos+2+len がセグメント末尾
            return Ok((pos + 10, pos + 2 + len));
        }
        pos += 2 + len;
    }
    bail!("APP1(Exif) セグメントが見つかりません")
}

/// TIFF 構造の読み取りヘルパ。オフセットはすべて TIFF ヘッダ (base) 相対。
struct Tiff<'a> {
    buf: &'a [u8],
    base: usize,
    end: usize,
    little_endian: bool,
}

/// IFD エントリ。`value_field` は 4 バイトの値/オフセット領域の base 相対オフセット。
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    value_field: usize,
}

impl<'a> Tiff<'a> {
    fn new(buf: &'a [u8], base: usize, end: usize) -> Result<Self> {
        ensure!(end <= buf.len() && end - base >= 8, "TIFF ヘッダが短すぎます");
        let little_endian = match &buf[base..base + 2] {
            b"II" => true,
            b"MM" => false,
            _ => bail!("TIFF バイトオーダーが不正です"),
        };
        let tiff = Tiff { buf, base, end, little_endian };
        ensure!(tiff.u16(2)? == 42, "TIFF マジックナンバーが不正です");
        Ok(tiff)
    }

    fn bytes(&self, rel: usize, n: usize) -> Result<&[u8]> {
        let abs = self.base + rel;
        ensure!(abs + n <= self.end, "TIFF 構造がセグメント外を指しています");
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

    /// ASCII 日時タグの値領域の絶対オフセットを返す。
    /// 日時は 19 文字 + NUL の 20 バイトで 4 バイトを超えるため、値は必ずオフセット参照。
    fn ascii_value_abs(&self, e: &Entry) -> Result<usize> {
        ensure!(e.typ == 2, "日時タグ 0x{:04X} が ASCII 型ではありません", e.tag);
        ensure!(
            e.count as usize >= DATETIME_LEN,
            "日時タグ 0x{:04X} の長さが不正です",
            e.tag
        );
        let off = self.u32(e.value_field)? as usize;
        let abs = self.base + off;
        ensure!(
            abs + DATETIME_LEN <= self.end,
            "日時タグ 0x{:04X} の値がセグメント外を指しています",
            e.tag
        );
        Ok(abs)
    }
}
