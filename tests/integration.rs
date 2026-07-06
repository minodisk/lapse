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
        assert!(
            result.is_ok(),
            "failed to process {}: {:?}",
            path.display(),
            result.as_ref().err()
        );
    }
}

/// Returns everything from the SOS marker on (the entropy-coded scan data and
/// EOI). Simplified implementation assuming 0xFFDA never appears inside APP1
/// in the sample JPEGs.
fn scan_data(buf: &[u8]) -> &[u8] {
    let pos = buf
        .windows(2)
        .position(|w| w == [0xFF, 0xDA])
        .expect("SOS marker not found");
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
        .unwrap_or_else(|| panic!("{tag} is missing"));
    match &field.value {
        exif::Value::Ascii(v) => String::from_utf8(v[0].clone()).unwrap(),
        other => panic!("{tag} is not ASCII: {other:?}"),
    }
}

fn assert_exif_datetime_format(s: &str) {
    assert_eq!(s.len(), 19, "date-time is not 19 bytes long: {s:?}");
    for (i, c) in s.char_indices() {
        match i {
            4 | 7 | 13 | 16 => assert_eq!(c, ':', "position {i} is not a colon: {s:?}"),
            10 => assert_eq!(c, ' ', "position 10 is not a space: {s:?}"),
            _ => assert!(c.is_ascii_digit(), "position {i} is not a digit: {s:?}"),
        }
    }
}

/// Test 1: lossless guarantee (the core guarantee of this tool).
/// Because of the in-place replacement approach, the file length must not
/// change and the differing bytes must be confined to the value areas of the
/// target date-time tags inside APP1 (19 bytes x 3 locations), verified byte
/// by byte. Additionally verifies that the scan data is identical and the
/// decoded pixel arrays match bit for bit.
#[test]
fn test_lossless_bytes_and_pixels() {
    let dir = setup(&["a.jpg", "b.jpg"]);
    let target = dir.path().join("b.jpg");
    let before = fs::read(&target).unwrap();

    run_all(dir.path());
    let after = fs::read(&target).unwrap();

    // b.jpg is index 1, so it gets +1 second and must differ
    assert_ne!(before, after, "no rewrite happened");
    assert_eq!(before.len(), after.len(), "file length changed");

    // Every differing byte must fall inside a target tag's value area
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
                "diff outside the date-time tags at position {i} (0x{b:02X} -> 0x{a:02X})"
            );
        }
    }

    // The scan data (entropy-coded section) must be byte-identical
    assert_eq!(
        scan_data(&before),
        scan_data(&after),
        "scan data changed (suspected re-encode)"
    );

    // The decoded pixel arrays must match bit for bit
    let pixels_before = image::load_from_memory(&before).unwrap().to_rgb8();
    let pixels_after = image::load_from_memory(&after).unwrap().to_rgb8();
    assert_eq!(
        pixels_before.as_raw(),
        pixels_after.as_raw(),
        "decoded pixels changed"
    );
}

/// Test 2: EXIF tags other than the date-times (all tags including Make,
/// Model, ExifImageWidth/Height, GPS, MakerNote) must not change across
/// processing.
#[test]
fn test_other_exif_tags_preserved() {
    let dir = setup(&["a.jpg", "b.jpg"]);
    let target = dir.path().join("b.jpg");
    let before_fields = exif_fields(&fs::read(&target).unwrap());

    run_all(dir.path());
    let after_fields = exif_fields(&fs::read(&target).unwrap());

    // The tag sets must match (no tags disappeared or appeared)
    let before_keys: Vec<_> = before_fields.keys().collect();
    let after_keys: Vec<_> = after_fields.keys().collect();
    assert_eq!(before_keys, after_keys, "tag set changed");

    // Values other than the date-time tags (DateTimeOriginal /
    // DateTimeDigitized / DateTime) must also match
    for (key, before_value) in &before_fields {
        if key.contains("DateTime") {
            continue;
        }
        assert_eq!(
            before_value, &after_fields[key],
            "non-date-time tag {key} changed"
        );
    }

    // Presence checks for the main tags and the GPS / MakerNote in the sample
    let has = |name: &str| before_fields.keys().any(|k| k.contains(name));
    let value_of = |name: &str| {
        after_fields
            .iter()
            .find(|(k, _)| k.contains(name))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("{name} is missing"))
    };
    assert!(value_of("Make").contains(common::MAKE));
    assert!(value_of("Model").contains(common::MODEL));
    assert_eq!(value_of("PixelXDimension"), common::WIDTH.to_string());
    assert_eq!(value_of("PixelYDimension"), common::HEIGHT.to_string());
    assert!(has("GPSLatitude"), "GPSLatitude disappeared");
    assert!(has("MakerNote"), "MakerNote disappeared");
}

/// Test 3: DateTimeOriginal of burst shots collapsed into the same second is
/// uniquified as "new time = max(original time, previous new time + 1 second)".
/// Also verifies natural sort (img_1 < img_2 < img_10), minute carry-over,
/// the YYYY:MM:DD HH:MM:SS format, and 0x9004 / 0x0132 sync.
#[test]
fn test_datetime_updated_in_natural_order() {
    let dir = setup(&["img_2.jpg", "img_10.jpg", "img_1.jpg"]);
    run_all(dir.path());

    // All files share the same second (2024:01:02 03:04:59), so they are
    // pushed to +0, +1, +2 seconds
    let expected = [
        ("img_1.jpg", "2024:01:02 03:04:59"),
        ("img_2.jpg", "2024:01:02 03:05:00"),
        ("img_10.jpg", "2024:01:02 03:05:01"),
    ];
    for (name, want) in expected {
        let buf = fs::read(dir.path().join(name)).unwrap();
        let dto = read_ascii_tag(&buf, exif::Tag::DateTimeOriginal);
        assert_eq!(dto, want, "unexpected DateTimeOriginal for {name}");
        assert_exif_datetime_format(&dto);
        // CreateDate (DateTimeDigitized) and ModifyDate (DateTime) are synced
        assert_eq!(read_ascii_tag(&buf, exif::Tag::DateTimeDigitized), want);
        assert_eq!(read_ascii_tag(&buf, exif::Tag::DateTime), want);
    }
}

/// Test 4: processed files must still decode as valid JPEGs.
#[test]
fn test_file_still_valid_jpeg() {
    let dir = setup(&["a.jpg", "b.jpg", "c.jpg"]);
    run_all(dir.path());

    for name in ["a.jpg", "b.jpg", "c.jpg"] {
        let buf = fs::read(dir.path().join(name)).unwrap();
        let img =
            image::load_from_memory(&buf).unwrap_or_else(|e| panic!("cannot decode {name}: {e}"));
        assert_eq!(img.width(), common::WIDTH);
        assert_eq!(img.height(), common::HEIGHT);
    }
}

/// With --dry-run, not a single byte of any file changes.
#[test]
fn test_dry_run_does_not_modify_files() {
    let dir = setup(&["a.jpg", "b.jpg"]);
    let before_a = fs::read(dir.path().join("a.jpg")).unwrap();
    let before_b = fs::read(dir.path().join("b.jpg")).unwrap();

    let summary = run(dir.path(), &Options { dry_run: true }).unwrap();
    assert!(summary.results.iter().all(|(_, r)| r.is_ok()));
    let outcome = summary.results[1].1.as_ref().unwrap();
    assert_eq!(outcome.new.to_string(), "2024:01:02 03:05:00");

    assert_eq!(before_a, fs::read(dir.path().join("a.jpg")).unwrap());
    assert_eq!(before_b, fs::read(dir.path().join("b.jpg")).unwrap());
}

/// Zero targets is a clear error; files without DateTimeOriginal are
/// collected as per-file errors.
#[test]
fn test_error_cases() {
    let empty = tempfile::tempdir().unwrap();
    let err = run(empty.path(), &Options { dry_run: false }).unwrap_err();
    assert!(err.to_string().contains("no target JPEG files"), "{err:#}");

    let no_exif = tempfile::tempdir().unwrap();
    fs::write(no_exif.path().join("a.jpg"), common::plain_jpeg()).unwrap();
    let summary = run(no_exif.path(), &Options { dry_run: false }).unwrap();
    assert_eq!(summary.results.len(), 1);
    // There is no EXIF segment at all, so the error says APP1 was not found
    let err = summary.results[0].1.as_ref().unwrap_err();
    assert!(format!("{err:#}").contains("Exif"), "{err:#}");
}

/// One file's failure does not stop the whole run; other files are processed
/// correctly.
#[test]
fn test_single_file_failure_does_not_stop_others() {
    let dir = setup(&["a.jpg", "c.jpg"]);
    fs::write(dir.path().join("b.jpg"), common::plain_jpeg()).unwrap();

    let summary = run(dir.path(), &Options { dry_run: false }).unwrap();
    assert_eq!(summary.results.len(), 3);
    assert!(summary.results[0].1.is_ok()); // a.jpg
    assert!(summary.results[1].1.is_err()); // b.jpg (no EXIF)
    assert!(summary.results[2].1.is_ok()); // c.jpg

    // b.jpg is excluded from assignment; c.jpg comes right after a.jpg (+1s)
    let buf = fs::read(dir.path().join("c.jpg")).unwrap();
    assert_eq!(
        read_ascii_tag(&buf, exif::Tag::DateTimeOriginal),
        "2024:01:02 03:05:00"
    );
}

/// Original times of separate scenes (photos with a time gap) are preserved,
/// and a scene is pushed back minimally only when the push-out catches up.
#[test]
fn test_scene_times_preserved_and_pushed_only_when_caught_up() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, dt: &str| {
        fs::write(dir.path().join(name), common::sample_jpeg_with(dt)).unwrap();
    };
    // 3 burst shots (same second) + one photo that gets caught up
    // + 2 photos of a scene far enough in the future
    write("img_1.jpg", "2024:01:02 03:04:59");
    write("img_2.jpg", "2024:01:02 03:04:59");
    write("img_3.jpg", "2024:01:02 03:04:59");
    write("img_4.jpg", "2024:01:02 03:05:00"); // caught up by the push-out
    write("img_5.jpg", "2024:01:02 04:00:00"); // preserved thanks to the gap
    write("img_6.jpg", "2024:01:02 04:00:00"); // same second as previous, so +1s
    run_all(dir.path());

    let expected = [
        ("img_1.jpg", "2024:01:02 03:04:59"), // first file keeps its original time
        ("img_2.jpg", "2024:01:02 03:05:00"),
        ("img_3.jpg", "2024:01:02 03:05:01"),
        ("img_4.jpg", "2024:01:02 03:05:02"), // originally 03:05:00 but caught up and pushed
        ("img_5.jpg", "2024:01:02 04:00:00"), // original time preserved
        ("img_6.jpg", "2024:01:02 04:00:01"),
    ];
    for (name, want) in expected {
        let buf = fs::read(dir.path().join(name)).unwrap();
        assert_eq!(
            read_ascii_tag(&buf, exif::Tag::DateTimeOriginal),
            want,
            "unexpected DateTimeOriginal for {name}"
        );
    }
}

/// Nothing changes in a directory whose times are already unique (idempotency).
#[test]
fn test_idempotent_when_already_unique() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("a.jpg"),
        common::sample_jpeg_with("2024:01:02 03:04:59"),
    )
    .unwrap();
    fs::write(
        dir.path().join("b.jpg"),
        common::sample_jpeg_with("2024:01:02 03:05:10"),
    )
    .unwrap();
    let before_a = fs::read(dir.path().join("a.jpg")).unwrap();
    let before_b = fs::read(dir.path().join("b.jpg")).unwrap();

    run_all(dir.path());

    assert_eq!(before_a, fs::read(dir.path().join("a.jpg")).unwrap());
    assert_eq!(before_b, fs::read(dir.path().join("b.jpg")).unwrap());
}

/// Extension filter: only .jpg / .jpeg (case-insensitive) are targeted; other
/// formats are ignored.
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

/// CLI (--dry-run) behavior: the planned timestamps are printed and the files
/// do not change.
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
    assert!(stdout.contains("2 file(s) would be rewritten"), "{stdout}");

    assert_eq!(before, fs::read(dir.path().join("img_2.jpg")).unwrap());
}
