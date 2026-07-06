//! テスト用サンプル JPEG の生成。
//! バイナリをリポジトリに同梱する代わりに、image crate で小さな JPEG を
//! エンコードし、手組みの EXIF APP1 セグメント（Make / Model / 日時 3 種 /
//! PixelXDimension / PixelYDimension / MakerNote / GPS IFD 付き）を挿入する。

use std::io::Cursor;

/// サンプルの基準時刻。分の繰り上がり（+1 秒で 03:05:00）も検証できる値にしている。
pub const BASE_DATETIME: &str = "2024:01:02 03:04:59";

pub const MAKE: &str = "TestMake";
pub const MODEL: &str = "TestModel";
pub const WIDTH: u32 = 16;
pub const HEIGHT: u32 = 16;

/// EXIF 付きサンプル JPEG を生成する。
pub fn sample_jpeg() -> Vec<u8> {
    sample_jpeg_with(BASE_DATETIME)
}

/// DateTimeOriginal を指定した EXIF 付きサンプル JPEG を生成する。
pub fn sample_jpeg_with(dt: &str) -> Vec<u8> {
    let base = plain_jpeg();
    let app1 = build_exif_app1(dt);
    // SOI 直後に APP1 を挿入する
    let mut out = Vec::with_capacity(base.len() + app1.len());
    out.extend_from_slice(&base[..2]);
    out.extend_from_slice(&app1);
    out.extend_from_slice(&base[2..]);
    out
}

/// EXIF なしの JPEG（エラーケース検証用）
pub fn plain_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_fn(WIDTH, HEIGHT, |x, y| {
        image::Rgb([(x * 16) as u8, (y * 16) as u8, 128])
    });
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
    let bytes = buf.into_inner();
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
    bytes
}

fn build_exif_app1(dt: &str) -> Vec<u8> {
    assert_eq!(dt.len(), 19);
    let mut dt20 = dt.as_bytes().to_vec();
    dt20.push(0);

    // TIFF レイアウト (リトルエンディアン):
    //   header(8) + IFD0(2+5*12+4=66) + ExifIFD(66) + GPSIFD(2+3*12+4=42) + データ領域
    const IFD0: u32 = 8;
    const EXIF_IFD: u32 = IFD0 + 66;
    const GPS_IFD: u32 = EXIF_IFD + 66;
    const DATA_START: u32 = GPS_IFD + 42;

    // データ領域に値を置き、TIFF ヘッダ相対オフセットを返す
    let mut data: Vec<u8> = Vec::new();
    let put = |data: &mut Vec<u8>, bytes: &[u8]| -> u32 {
        if data.len() % 2 == 1 {
            data.push(0); // ワード境界に揃える
        }
        let off = DATA_START + data.len() as u32;
        data.extend_from_slice(bytes);
        off
    };

    let make = format!("{MAKE}\0");
    let model = format!("{MODEL}\0");
    let make_off = put(&mut data, make.as_bytes());
    let model_off = put(&mut data, model.as_bytes());
    let dt_off = put(&mut data, &dt20);
    let dto_off = put(&mut data, &dt20);
    let dtd_off = put(&mut data, &dt20);
    let maker_note = b"MKNOTE\x01\x02";
    let maker_off = put(&mut data, maker_note);
    let mut gps_lat = Vec::new();
    for (num, den) in [(35u32, 1u32), (41, 1), (1234, 100)] {
        gps_lat.extend_from_slice(&num.to_le_bytes());
        gps_lat.extend_from_slice(&den.to_le_bytes());
    }
    let gps_lat_off = put(&mut data, &gps_lat);

    fn entry(out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]) {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value);
    }
    fn off4(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&IFD0.to_le_bytes());

    // IFD0: Make, Model, DateTime, ExifIFD ポインタ, GPSIFD ポインタ
    tiff.extend_from_slice(&5u16.to_le_bytes());
    entry(&mut tiff, 0x010F, 2, make.len() as u32, off4(make_off));
    entry(&mut tiff, 0x0110, 2, model.len() as u32, off4(model_off));
    entry(&mut tiff, 0x0132, 2, 20, off4(dt_off));
    entry(&mut tiff, 0x8769, 4, 1, off4(EXIF_IFD));
    entry(&mut tiff, 0x8825, 4, 1, off4(GPS_IFD));
    tiff.extend_from_slice(&0u32.to_le_bytes());

    // Exif IFD: DateTimeOriginal, DateTimeDigitized, MakerNote, PixelX/YDimension
    tiff.extend_from_slice(&5u16.to_le_bytes());
    entry(&mut tiff, 0x9003, 2, 20, off4(dto_off));
    entry(&mut tiff, 0x9004, 2, 20, off4(dtd_off));
    entry(&mut tiff, 0x927C, 7, maker_note.len() as u32, off4(maker_off));
    entry(&mut tiff, 0xA002, 4, 1, off4(WIDTH));
    entry(&mut tiff, 0xA003, 4, 1, off4(HEIGHT));
    tiff.extend_from_slice(&0u32.to_le_bytes());

    // GPS IFD: GPSVersionID, GPSLatitudeRef, GPSLatitude
    tiff.extend_from_slice(&3u16.to_le_bytes());
    entry(&mut tiff, 0x0000, 1, 4, [2, 3, 0, 0]);
    entry(&mut tiff, 0x0001, 2, 2, *b"N\0\0\0");
    entry(&mut tiff, 0x0002, 5, 3, off4(gps_lat_off));
    tiff.extend_from_slice(&0u32.to_le_bytes());

    assert_eq!(tiff.len() as u32, DATA_START);
    tiff.extend_from_slice(&data);

    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    app1.extend_from_slice(&payload);
    app1
}
