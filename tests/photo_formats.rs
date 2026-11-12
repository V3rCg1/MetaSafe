use std::{fs, io::Cursor};

use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use metasafe::core::{
    self,
    metadata::{MetadataKind, Status},
};

fn encoded(format: ImageFormat) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(12, 8, Rgb([23, 87, 153])));
    let image = if format == ImageFormat::Ico {
        DynamicImage::ImageRgba8(image.into_rgba8())
    } else {
        image
    };
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(data);
    chunk.extend_from_slice(&crc32fast::hash(&chunk[4..]).to_be_bytes());
    chunk
}

#[test]
fn reads_png_text_and_removes_it_from_a_transparent_copy() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("input.png");
    let output = directory.path().join("copy.png");
    let rejected = directory.path().join("copy.jpg");
    let pixels = RgbaImage::from_pixel(12, 8, Rgba([12, 20, 40, 61]));
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(pixels.clone())
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    let mut bytes = bytes.into_inner();
    let metadata = png_chunk(b"tEXt", b"Author\0Local test name");
    bytes.splice(33..33, metadata);
    fs::write(&source, &bytes).unwrap();

    let (report, preview) = core::preview(&source).unwrap();
    assert_eq!(report.format, "PNG");
    assert_eq!(preview, pixels);
    assert!(report.details.iter().any(|field| {
        field.kind == MetadataKind::Comment
            && field.name == "Author"
            && field.value == "Local test name"
    }));
    assert!(!core::verify(&source).unwrap().passed());
    assert!(matches!(
        core::clean(&source, &rejected),
        Err(core::Error::TransparentJpeg)
    ));
    assert!(!rejected.exists());

    assert!(core::clean(&source, &output).unwrap().passed());
    assert_eq!(fs::read(&source).unwrap(), bytes);
    assert_eq!(image::open(&output).unwrap().into_rgba8(), pixels);
    assert!(core::verify(&output).unwrap().details.is_empty());
}

#[test]
fn opens_and_cleans_common_non_jpeg_formats() {
    let directory = tempfile::tempdir().unwrap();
    for (format, extension) in [
        (ImageFormat::WebP, "webp"),
        (ImageFormat::Tiff, "tiff"),
        (ImageFormat::Gif, "gif"),
        (ImageFormat::Bmp, "bmp"),
        (ImageFormat::Ico, "ico"),
        (ImageFormat::Tga, "tga"),
        (ImageFormat::Qoi, "qoi"),
        (ImageFormat::Pnm, "ppm"),
    ] {
        let source = directory.path().join(format!("original.{extension}"));
        let output = directory.path().join(format!("{extension}_clean.png"));
        fs::write(&source, encoded(format)).unwrap();
        let (_, preview) =
            core::preview(&source).unwrap_or_else(|error| panic!("{extension}: {error}"));
        assert_eq!(preview.dimensions(), (12, 8), "{extension}");
        let cleaned =
            core::clean(&source, &output).unwrap_or_else(|error| panic!("{extension}: {error}"));
        assert_eq!(cleaned.format, "PNG");
        assert!(cleaned.passed(), "{extension}");
    }
}

#[test]
fn mixed_batch_names_outputs_by_format() {
    let directory = tempfile::tempdir().unwrap();
    let files = [
        directory.path().join("photo.jpg"),
        directory.path().join("photo.webp"),
    ];
    fs::write(&files[0], encoded(ImageFormat::Jpeg)).unwrap();
    fs::write(&files[1], encoded(ImageFormat::WebP)).unwrap();
    let batch = core::batch_clean(&files, directory.path(), |_| {}).unwrap();
    assert!(
        batch
            .items
            .iter()
            .all(|item| item.result.as_ref().is_ok_and(|report| report.passed()))
    );
    assert!(directory.path().join("photo_clean.jpg").is_file());
    assert!(directory.path().join("photo_clean.png").is_file());
}

#[test]
fn jpeg_comment_is_a_disclosable_field() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.jpg");
    let comment = b"Local test comment";
    let mut segment = vec![0xff, 0xfe];
    segment.extend_from_slice(&((comment.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(comment);
    let mut bytes = encoded(ImageFormat::Jpeg);
    bytes.splice(2..2, segment);
    fs::write(&source, bytes).unwrap();
    let report = core::inspect(&source).unwrap();
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.kind == MetadataKind::Comment && check.status == Status::Present)
    );
    assert!(report.details.iter().any(|field| field.kind == MetadataKind::Comment && field.value == "Local test comment"));
}

#[test]
fn animated_gif_reports_comment_and_exports_first_frame() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("animation.gif");
    let output = directory.path().join("first.png");
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        for color in [[10, 20, 30, 255], [220, 190, 160, 255]] {
            encoder
                .encode_frame(image::Frame::new(RgbaImage::from_pixel(4, 3, Rgba(color))))
                .unwrap();
        }
    }
    let end = bytes.len() - 1;
    bytes.splice(end..end, [0x21, 0xfe, 4, b't', b'e', b's', b't', 0]);
    fs::write(&source, bytes).unwrap();
    let (report, _) = core::preview(&source).unwrap();
    assert!(report.single_frame_export);
    assert!(
        report
            .details
            .iter()
            .any(|detail| detail.kind == MetadataKind::Comment && detail.value == "test")
    );
    assert!(core::clean(&source, &output).unwrap().passed());
    assert_eq!(
        image::open(&output).unwrap().into_rgba8().get_pixel(0, 0).0,
        [10, 20, 30, 255]
    );
}
