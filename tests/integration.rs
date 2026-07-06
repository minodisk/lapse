mod common;

use lapse::{run, Options};
use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::Path;

fn setup(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in names {
        fs::write(dir.path().join(name), common::sample_jpeg()).unwrap();
    }
    dir
}

fn run_all(dir: &Path) {
    let summary = run(dir, &Options { dry_run: false }).unwrap();
    for (path, result) in &summary.results {
        assert!(result.is_ok(), "{} の処理に失敗: {:?}", path.display(), result.as_ref().err());
    }
}

/// SOS マーカー以降（エントロピー符号化されたスキャンデータと EOI）を返す。
/// サンプル JPEG では APP1 内に 0xFFDA が現れないことを前提にした簡易実装。
fn scan_data(buf: &[u8]) -> &[u8] {
    let pos = buf
        .windows(2)
        .position(|w| w == [0xFF, 0xDA])
        .expect("SOS マーカーが見つからない");
    &buf[pos..]
}

fn exif_fields(buf: &[u8]) -> BTreeMap<String, String> {
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(buf))
        .unwrap();
    exif.fields()
        .map(|f| {
            (
                format!("{:?}/{}", f.ifd_num, f.tag),
                f.display_value().to_string(),
            )
        })
        .collect()
}

fn read_ascii_tag(buf: &[u8], tag: exif::Tag) -> String {
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(buf))
        .unwrap();
    let field = exif
        .get_field(tag, exif::In::PRIMARY)
        .unwrap_or_else(|| panic!("{tag} がない"));
    match &field.value {
        exif::Value::Ascii(v) => String::from_utf8(v[0].clone()).unwrap(),
        other => panic!("{tag} が ASCII ではない: {other:?}"),
    }
}

fn assert_exif_datetime_format(s: &str) {
    assert_eq!(s.len(), 19, "日時の長さが 19 バイトでない: {s:?}");
    for (i, c) in s.char_indices() {
        match i {
            4 | 7 | 13 | 16 => assert_eq!(c, ':', "位置 {i} がコロンでない: {s:?}"),
            10 => assert_eq!(c, ' ', "位置 10 が空白でない: {s:?}"),
            _ => assert!(c.is_ascii_digit(), "位置 {i} が数字でない: {s:?}"),
        }
    }
}

/// テスト 1: 無劣化保証（本ツールの中核的保証）。
/// インプレース置換方式なので、処理前後でファイル長が変わらず、
/// 差分バイトが APP1 内の対象日時タグの値領域（19 バイト × 3 箇所）のみに
/// 収まることをバイト単位で検証する。加えてスキャンデータの完全一致と、
/// デコード後のピクセル配列のビット単位一致も確認する。
#[test]
fn test_lossless_bytes_and_pixels() {
    let dir = setup(&["a.jpg", "b.jpg"]);
    let target = dir.path().join("b.jpg");
    let before = fs::read(&target).unwrap();

    run_all(dir.path());
    let after = fs::read(&target).unwrap();

    // b.jpg はインデックス 1 なので +1 秒され、必ず差分が生じる
    assert_ne!(before, after, "書き換えが行われていない");
    assert_eq!(before.len(), after.len(), "ファイル長が変化した");

    // 差分バイトの位置がすべて対象タグの値領域に収まっていること
    let offsets = lapse::exif_patch::find_datetime_offsets(&before).unwrap();
    let allowed: Vec<std::ops::Range<usize>> = offsets
        .all()
        .into_iter()
        .map(|o| o..o + lapse::exif_patch::DATETIME_LEN)
        .collect();
    for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
        if b != a {
            assert!(
                allowed.iter().any(|r| r.contains(&i)),
                "日時タグ以外の位置 {i} に差分がある (0x{b:02X} -> 0x{a:02X})"
            );
        }
    }

    // スキャンデータ（エントロピー符号化部分）がバイト単位で完全一致すること
    assert_eq!(
        scan_data(&before),
        scan_data(&after),
        "スキャンデータが変化した（再エンコードの疑い）"
    );

    // デコード後のピクセル配列がビット単位で一致すること
    let pixels_before = image::load_from_memory(&before).unwrap().to_rgb8();
    let pixels_after = image::load_from_memory(&after).unwrap().to_rgb8();
    assert_eq!(
        pixels_before.as_raw(),
        pixels_after.as_raw(),
        "デコード後のピクセルが変化した"
    );
}

/// テスト 2: 日時タグ以外の EXIF タグ（Make, Model, ExifImageWidth/Height,
/// GPS, MakerNote を含む全タグ）が処理前後で変化しないこと。
#[test]
fn test_other_exif_tags_preserved() {
    let dir = setup(&["a.jpg", "b.jpg"]);
    let target = dir.path().join("b.jpg");
    let before_fields = exif_fields(&fs::read(&target).unwrap());

    run_all(dir.path());
    let after_fields = exif_fields(&fs::read(&target).unwrap());

    // タグ集合が一致（消えたタグ・増えたタグがない）こと
    let before_keys: Vec<_> = before_fields.keys().collect();
    let after_keys: Vec<_> = after_fields.keys().collect();
    assert_eq!(before_keys, after_keys, "タグ集合が変化した");

    // 日時タグ (DateTimeOriginal / DateTimeDigitized / DateTime) 以外は値も一致すること
    for (key, before_value) in &before_fields {
        if key.contains("DateTime") {
            continue;
        }
        assert_eq!(
            before_value, &after_fields[key],
            "日時以外のタグ {key} の値が変化した"
        );
    }

    // 主要タグとサンプルに含めた GPS / MakerNote の存在確認
    let has = |name: &str| before_fields.keys().any(|k| k.contains(name));
    let value_of = |name: &str| {
        after_fields
            .iter()
            .find(|(k, _)| k.contains(name))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("{name} がない"))
    };
    assert!(value_of("Make").contains(common::MAKE));
    assert!(value_of("Model").contains(common::MODEL));
    assert_eq!(value_of("PixelXDimension"), common::WIDTH.to_string());
    assert_eq!(value_of("PixelYDimension"), common::HEIGHT.to_string());
    assert!(has("GPSLatitude"), "GPSLatitude が消えた");
    assert!(has("MakerNote"), "MakerNote が消えた");
}

/// テスト 3: DateTimeOriginal が「基準時刻 + インデックス秒」に正しく書き換わること。
/// 自然順ソート (img_1 < img_2 < img_10) と分の繰り上がり、
/// YYYY:MM:DD HH:MM:SS フォーマット、0x9004 / 0x0132 の同期も検証する。
#[test]
fn test_datetime_updated_in_natural_order() {
    let dir = setup(&["img_2.jpg", "img_10.jpg", "img_1.jpg"]);
    run_all(dir.path());

    // 基準時刻 2024:01:02 03:04:59 に対して +0, +1, +2 秒
    let expected = [
        ("img_1.jpg", "2024:01:02 03:04:59"),
        ("img_2.jpg", "2024:01:02 03:05:00"),
        ("img_10.jpg", "2024:01:02 03:05:01"),
    ];
    for (name, want) in expected {
        let buf = fs::read(dir.path().join(name)).unwrap();
        let dto = read_ascii_tag(&buf, exif::Tag::DateTimeOriginal);
        assert_eq!(dto, want, "{name} の DateTimeOriginal が期待値と異なる");
        assert_exif_datetime_format(&dto);
        // CreateDate (DateTimeDigitized) と ModifyDate (DateTime) も同じ値に揃う
        assert_eq!(read_ascii_tag(&buf, exif::Tag::DateTimeDigitized), want);
        assert_eq!(read_ascii_tag(&buf, exif::Tag::DateTime), want);
    }
}

/// テスト 4: 処理後のファイルが依然として有効な JPEG としてデコードできること。
#[test]
fn test_file_still_valid_jpeg() {
    let dir = setup(&["a.jpg", "b.jpg", "c.jpg"]);
    run_all(dir.path());

    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        let buf = fs::read(dir.path().join(name)).unwrap();
        let img = image::load_from_memory(&buf)
            .unwrap_or_else(|e| panic!("{name} をデコードできない: {e}"));
        assert_eq!(img.width(), common::WIDTH);
        assert_eq!(img.height(), common::HEIGHT);
    }
}

/// --dry-run ではファイルが 1 バイトも変化しないこと。
#[test]
fn test_dry_run_does_not_modify_files() {
    let dir = setup(&["a.jpg", "b.jpg"]);
    let before_a = fs::read(dir.path().join("a.jpg")).unwrap();
    let before_b = fs::read(dir.path().join("b.jpg")).unwrap();

    let summary = run(dir.path(), &Options { dry_run: true }).unwrap();
    assert!(summary.results.iter().all(|(_, r)| r.is_ok()));
    let outcome = summary.results[1].1.as_ref().unwrap();
    assert_eq!(outcome.new, "2024:01:02 03:05:00");

    assert_eq!(before_a, fs::read(dir.path().join("a.jpg")).unwrap());
    assert_eq!(before_b, fs::read(dir.path().join("b.jpg")).unwrap());
}

/// 対象 0 件・先頭ファイルに DateTimeOriginal なし、は明確なエラーになること。
#[test]
fn test_error_cases() {
    let empty = tempfile::tempdir().unwrap();
    let err = run(empty.path(), &Options { dry_run: false }).unwrap_err();
    assert!(err.to_string().contains("見つかりません"), "{err:#}");

    let no_exif = tempfile::tempdir().unwrap();
    fs::write(no_exif.path().join("a.jpg"), common::plain_jpeg()).unwrap();
    let err = run(no_exif.path(), &Options { dry_run: false }).unwrap_err();
    assert!(format!("{err:#}").contains("DateTimeOriginal"), "{err:#}");
}

/// 先頭以外のファイルの失敗が全体を止めず、他ファイルは正しく処理されること。
#[test]
fn test_single_file_failure_does_not_stop_others() {
    let dir = setup(&["a.jpg", "c.jpg"]);
    fs::write(dir.path().join("b.jpg"), common::plain_jpeg()).unwrap();

    let summary = run(dir.path(), &Options { dry_run: false }).unwrap();
    assert_eq!(summary.results.len(), 3);
    assert!(summary.results[0].1.is_ok()); // a.jpg
    assert!(summary.results[1].1.is_err()); // b.jpg (EXIF なし)
    assert!(summary.results[2].1.is_ok()); // c.jpg

    // c.jpg はインデックス 2 なので基準 +2 秒
    let buf = fs::read(dir.path().join("c.jpg")).unwrap();
    assert_eq!(
        read_ascii_tag(&buf, exif::Tag::DateTimeOriginal),
        "2024:01:02 03:05:01"
    );
}

/// 拡張子フィルタ: .jpg / .jpeg（大文字小文字不問）のみ対象で、他形式は無視されること。
#[test]
fn test_collect_jpegs_filters_and_sorts() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["b.JPG", "a.jpeg", "notes.txt", "raw.CR2", "img.png"] {
        fs::write(dir.path().join(name), b"dummy").unwrap();
    }
    let files = lapse::collect_jpegs(dir.path()).unwrap();
    let names: Vec<_> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.jpeg", "b.JPG"]);
}

/// CLI (--dry-run) の動作確認: 予定タイムスタンプが表示され、ファイルは変化しないこと。
#[test]
fn test_cli_dry_run() {
    let dir = setup(&["img_1.jpg", "img_2.jpg"]);
    let before = fs::read(dir.path().join("img_2.jpg")).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lapse"))
        .arg(dir.path())
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("img_1.jpg"), "{stdout}");
    assert!(stdout.contains("2024:01:02 03:05:00"), "{stdout}");
    assert!(stdout.contains("2 件のファイルを書き換え予定"), "{stdout}");

    assert_eq!(before, fs::read(dir.path().join("img_2.jpg")).unwrap());
}
