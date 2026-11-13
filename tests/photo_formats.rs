use std::{
    fs,
    io::{Cursor, Write},
};

use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage, metadata::Orientation};
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

#[test]
fn no_reencode_batch_preserves_compressed_jpeg_png_and_webp() {
    let directory = tempfile::tempdir().unwrap();
    let jpeg = directory.path().join("photo.jpg");
    let png = directory.path().join("photo.png");
    let webp = directory.path().join("photo.webp");
    let gif = directory.path().join("photo.gif");

    let jpeg_clean = encoded(ImageFormat::Jpeg);
    let mut jpeg_input = jpeg_clean.clone();
    jpeg_input.splice(2..2, [0xff, 0xfe, 0, 6, b't', b'e', b's', b't']);
    jpeg_input.extend_from_slice(b"private trailer");
    fs::write(&jpeg, &jpeg_input).unwrap();

    let png_clean = encoded(ImageFormat::Png);
    let mut png_input = png_clean.clone();
    png_input.splice(33..33, png_chunk(b"tEXt", b"Author\0Private author"));
    fs::write(&png, &png_input).unwrap();

    fn webp_with_xmp(image: &[u8], metadata: bool) -> Vec<u8> {
        let mut file = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
        file.extend_from_slice(&[
            if metadata { 0x14 } else { 0x10 },
            0,
            0,
            0,
            11,
            0,
            0,
            7,
            0,
            0,
        ]);
        file.extend_from_slice(&image[12..]);
        if metadata {
            file.extend_from_slice(b"XMP \x04\0\0\0test");
        }
        let riff_size = (file.len() as u32 - 8).to_le_bytes();
        file[4..8].copy_from_slice(&riff_size);
        file
    }

    let transparent =
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(12, 8, Rgba([23, 87, 153, 120])));
    let mut webp_encoded = Cursor::new(Vec::new());
    transparent
        .write_to(&mut webp_encoded, ImageFormat::WebP)
        .unwrap();
    let webp_encoded = webp_encoded.into_inner();
    let webp_input = webp_with_xmp(&webp_encoded, true);
    let webp_clean = webp_with_xmp(&webp_encoded, false);
    fs::write(&webp, &webp_input).unwrap();
    fs::write(&gif, encoded(ImageFormat::Gif)).unwrap();

    let result = std::process::Command::new(env!("CARGO_BIN_EXE_metasafe"))
        .args(["batch", "--no-reencode"])
        .args([&jpeg, &gif, &png, &webp])
        .arg("--output-dir")
        .arg(directory.path())
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&result.stdout).contains("Created 3/4 copies"));
    for (extension, original, before, expected) in [
        ("jpg", jpeg, jpeg_input, jpeg_clean),
        ("png", png, png_input, png_clean),
        ("webp", webp, webp_input, webp_clean),
    ] {
        let output = directory.path().join(format!("photo_clean.{extension}"));
        assert_eq!(fs::read(&original).unwrap(), before);
        assert_eq!(fs::read(&output).unwrap(), expected);
        assert!(core::verify(&output).unwrap().passed());
    }
}

fn tiff_orientation(orientation: u8) -> [u8; 26] {
    [
        b'I',
        b'I',
        42,
        0,
        8,
        0,
        0,
        0, // Little-endian TIFF and offset to IFD0.
        1,
        0,
        0x12,
        0x01,
        3,
        0,
        1,
        0,
        0,
        0, // One SHORT Orientation entry.
        orientation,
        0,
        0,
        0,
        0,
        0,
        0,
        0, // Value and end of IFD chain.
    ]
}

fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut segment = vec![0xff, marker];
    segment.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    segment.extend_from_slice(payload);
    segment
}

fn riff_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut chunk = kind.to_vec();
    chunk.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    chunk.extend_from_slice(payload);
    if !payload.len().is_multiple_of(2) {
        chunk.push(0);
    }
    chunk
}

fn source_webp(encoded: &[u8], orientation: u8) -> (Vec<u8>, Vec<u8>) {
    // VP8L carries alpha, so VP8X describes the same canvas with metadata flags.
    let mut clean = b"RIFF\0\0\0\0WEBP".to_vec();
    clean.extend(riff_chunk(b"VP8X", &[0x10, 0, 0, 0, 11, 0, 0, 7, 0, 0]));
    clean.extend_from_slice(&encoded[12..]);
    let length = (clean.len() as u32 - 8).to_le_bytes();
    clean[4..8].copy_from_slice(&length);
    let mut tagged = clean.clone();
    tagged[20] |= 0x2c; // ICC, EXIF and XMP flags.
    // The profile payload is opaque to MetaSafe and serves as a synthetic tag.
    tagged.splice(30..30, riff_chunk(b"ICCP", b"synthetic profile"));
    tagged.extend(riff_chunk(b"EXIF", &tiff_orientation(orientation)));
    tagged.extend(riff_chunk(
        b"XMP ",
        b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>",
    ));
    let length = (tagged.len() as u32 - 8).to_le_bytes();
    tagged[4..8].copy_from_slice(&length);
    (tagged, clean)
}

#[test]
fn complex_photo_metadata_is_disclosed_and_cleaned_without_altering_sources() {
    let directory = tempfile::tempdir().unwrap();
    let pixels = RgbaImage::from_fn(12, 8, |x, y| Rgba([x as u8 * 12, y as u8 * 20, 150, 120]));
    let image = DynamicImage::ImageRgba8(pixels.clone());
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png).unwrap();
    let clean_png = png.into_inner();

    let mut compressed =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    compressed.write_all(b"Synthetic compressed note").unwrap();
    let mut compressed_text = b"Description\0\0".to_vec();
    compressed_text.extend(compressed.finish().unwrap());
    let mut png_metadata = png_chunk(b"tEXt", b"Author\0Synthetic author");
    png_metadata.extend(png_chunk(b"zTXt", &compressed_text));
    png_metadata.extend(png_chunk(
        b"iTXt",
        b"XML:com.adobe.xmp\0\0\0\0\0<x:xmpmeta xmlns:x='adobe:ns:meta/'/>",
    ));
    let mut tagged_png = clean_png.clone();
    tagged_png.splice(33..33, png_metadata);

    let mut webp = Cursor::new(Vec::new());
    image.write_to(&mut webp, ImageFormat::WebP).unwrap();
    let (tagged_webp, clean_webp) = source_webp(&webp.into_inner(), 1);

    let clean_jpeg = encoded(ImageFormat::Jpeg);
    let mut tagged_jpeg = clean_jpeg.clone();
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(&tiff_orientation(1));
    let mut jpeg_metadata = jpeg_segment(0xe1, &exif);
    jpeg_metadata.extend(jpeg_segment(
        0xe1,
        b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x='adobe:ns:meta/'/>",
    ));
    jpeg_metadata.extend(jpeg_segment(0xfe, b"Synthetic JPEG comment"));
    tagged_jpeg.splice(2..2, jpeg_metadata);

    for (extension, tagged, clean, groups) in [
        (
            "jpg",
            tagged_jpeg,
            clean_jpeg,
            &[MetadataKind::Exif, MetadataKind::Xmp, MetadataKind::Comment][..],
        ),
        (
            "png",
            tagged_png,
            clean_png,
            &[MetadataKind::Comment, MetadataKind::Xmp][..],
        ),
        (
            "webp",
            tagged_webp,
            clean_webp,
            &[MetadataKind::Exif, MetadataKind::Xmp, MetadataKind::Icc][..],
        ),
    ] {
        let source = directory.path().join(format!("source.{extension}"));
        let destination = directory.path().join(format!("clean.{extension}"));
        fs::write(&source, &tagged).unwrap();
        let inspection = core::inspect(&source).unwrap();
        for kind in groups {
            assert!(
                inspection
                    .checks
                    .iter()
                    .any(|check| check.kind == *kind && check.status == Status::Present)
            );
        }
        if extension == "png" {
            for text in ["Synthetic author", "Synthetic compressed note"] {
                assert!(inspection.details.iter().any(|detail| detail.value == text));
            }
        }
        assert!(!core::verify(&source).unwrap().passed());
        assert!(
            core::clean_with_mode(&source, &destination, core::CleanMode::KeepEncoding)
                .unwrap()
                .passed()
        );
        assert_eq!(fs::read(&destination).unwrap(), clean);
        let expected_pixels = if extension == "jpg" {
            image::load_from_memory_with_format(&clean, ImageFormat::Jpeg)
                .unwrap()
                .into_rgba8()
        } else {
            pixels.clone()
        };
        assert_eq!(
            image::open(&destination).unwrap().into_rgba8(),
            expected_pixels
        );
        assert!(core::verify(&destination).unwrap().passed());
        assert!(matches!(
            core::clean_with_mode(&source, &destination, core::CleanMode::KeepEncoding),
            Err(core::Error::OutputExists(_))
        ));
        assert_eq!(fs::read(&destination).unwrap(), clean);
        assert_eq!(fs::read(&source).unwrap(), tagged);
    }
}

#[test]
fn webp_with_orientation_requires_pixel_transform() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.webp");
    let rejected = directory.path().join("lossless.webp");
    let destination = directory.path().join("oriented.png");
    let pixels = RgbaImage::from_fn(12, 8, |x, y| Rgba([x as u8 * 12, y as u8 * 20, 150, 120]));
    let mut expected = DynamicImage::ImageRgba8(pixels);
    let mut bytes = Cursor::new(Vec::new());
    expected.write_to(&mut bytes, ImageFormat::WebP).unwrap();
    let (tagged, _) = source_webp(&bytes.into_inner(), 6);
    fs::write(&source, &tagged).unwrap();
    expected.apply_orientation(Orientation::Rotate90);

    assert_eq!(core::inspect(&source).unwrap().orientation, Some(6));
    assert!(matches!(
        core::clean_with_mode(&source, &rejected, core::CleanMode::KeepEncoding),
        Err(core::Error::LosslessTransformRequired)
    ));
    assert!(!rejected.exists());
    assert!(core::clean(&source, &destination).unwrap().passed());
    assert_eq!(
        image::open(&destination).unwrap().into_rgba8(),
        expected.into_rgba8()
    );
    assert_eq!(fs::read(&source).unwrap(), tagged);
}
