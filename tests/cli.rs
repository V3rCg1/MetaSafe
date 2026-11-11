use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use image::{Rgb, RgbImage, codecs::jpeg::JpegEncoder};
use tempfile::tempdir;

fn metasafe(command: &str, file: &Path, output: Option<&Path>) -> Output {
    let mut process = Command::new(env!("CARGO_BIN_EXE_metasafe"));
    process.arg(command).arg(file);
    if let Some(output) = output {
        process.arg("--output").arg(output);
    }
    process.output().expect("run MetaSafe CLI")
}

fn batch(sources: &[&Path], output_dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_metasafe"))
        .arg("batch")
        .args(sources)
        .arg("--output-dir")
        .arg(output_dir)
        .output()
        .expect("run MetaSafe batch")
}

fn jpeg_with_comment() -> Vec<u8> {
    let pixels = RgbImage::from_pixel(8, 5, Rgb([36, 80, 121]));
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&pixels)
        .unwrap();
    jpeg.splice(2..2, [0xff, 0xfe, 0x00, 0x05, b'M', b'S', b'!']);
    jpeg
}

#[test]
fn inspect_clean_and_verify_preserve_the_source() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("Исходное фото.jpeg");
    let destination = dir.path().join("Очищенная копия.jpeg");
    let original = jpeg_with_comment();
    fs::write(&source, &original).unwrap();

    let inspection = metasafe("inspect", &source, None);
    assert!(inspection.status.success(), "{inspection:?}");
    assert!(inspection.stderr.is_empty(), "{inspection:?}");
    let inspection_text = String::from_utf8(inspection.stdout).unwrap();
    assert!(inspection_text.contains("Исходное фото.jpeg"));
    assert!(inspection_text.contains("Metadata checks:"));
    assert!(inspection_text.contains("JPEG comments: detected"));

    let findings = metasafe("verify", &source, None);
    assert_eq!(findings.status.code(), Some(1), "{findings:?}");
    assert!(findings.stderr.is_empty(), "{findings:?}");

    let cleaning = metasafe("clean", &source, Some(&destination));
    assert!(cleaning.status.success(), "{cleaning:?}");
    assert!(cleaning.stderr.is_empty(), "{cleaning:?}");
    assert_eq!(fs::read(&source).unwrap(), original);

    let verification = metasafe("verify", &destination, None);
    assert!(verification.status.success(), "{verification:?}");
    assert!(verification.stderr.is_empty(), "{verification:?}");
    assert!(
        String::from_utf8(verification.stdout)
            .unwrap()
            .contains("All supported checks passed.")
    );

    let cleaned = fs::read(&destination).unwrap();
    let conflict = metasafe("clean", &source, Some(&destination));
    assert_eq!(conflict.status.code(), Some(2), "{conflict:?}");
    assert!(conflict.stdout.is_empty());
    assert!(!conflict.stderr.is_empty());
    assert_eq!(fs::read(&destination).unwrap(), cleaned);

    let same_path = metasafe("clean", &source, Some(&source));
    assert_eq!(same_path.status.code(), Some(2), "{same_path:?}");
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[test]
fn invalid_jpeg_returns_error_status_and_creates_no_copy() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("invalid.jpg");
    let destination = dir.path().join("copy.jpg");
    fs::write(&source, b"not a JPEG").unwrap();

    let verification = metasafe("verify", &source, None);
    assert_eq!(verification.status.code(), Some(2), "{verification:?}");
    assert!(verification.stdout.is_empty());
    assert!(!verification.stderr.is_empty());

    let cleaning = metasafe("clean", &source, Some(&destination));
    assert_eq!(cleaning.status.code(), Some(2), "{cleaning:?}");
    assert!(!destination.exists());
}

#[test]
fn clean_applies_all_exif_orientations() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("source.jpg");
    let colors = [[240, 24, 20], [22, 226, 45], [25, 45, 234], [225, 227, 22]];
    let quadrants = RgbImage::from_fn(48, 32, |x, y| {
        let index = usize::from(x >= 24) + 2 * usize::from(y >= 16);
        Rgb(colors[index])
    });
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 95)
        .encode_image(&quadrants)
        .unwrap();

    // Expected top-left, top-right, bottom-left, and bottom-right quadrants.
    let expected = [
        [0, 1, 2, 3],
        [1, 0, 3, 2],
        [3, 2, 1, 0],
        [2, 3, 0, 1],
        [0, 2, 1, 3],
        [2, 0, 3, 1],
        [3, 1, 2, 0],
        [1, 3, 0, 2],
    ];
    for (index, layout) in expected.into_iter().enumerate() {
        let orientation = (index + 1) as u8;
        let mut original = jpeg.clone();
        // APP1 contains a little-endian TIFF IFD with one SHORT tag, Orientation.
        original.splice(
            2..2,
            [
                0xff,
                0xe1,
                0,
                34,
                b'E',
                b'x',
                b'i',
                b'f',
                0,
                0,
                b'I',
                b'I',
                42,
                0,
                8,
                0,
                0,
                0,
                1,
                0,
                0x12,
                0x01,
                3,
                0,
                1,
                0,
                0,
                0,
                orientation,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
        fs::write(&source, &original).unwrap();
        let destination = dir.path().join(format!("clean-{orientation}.jpg"));

        let inspection = metasafe("inspect", &source, None);
        assert!(inspection.status.success(), "{inspection:?}");
        assert!(
            String::from_utf8(inspection.stdout)
                .unwrap()
                .contains(&format!("EXIF orientation: {orientation}"))
        );

        let cleaning = metasafe("clean", &source, Some(&destination));
        assert!(
            cleaning.status.success(),
            "orientation {orientation}: {cleaning:?}"
        );
        assert_eq!(fs::read(&source).unwrap(), original);

        let cleaned = image::open(destination).unwrap().into_rgb8();
        let (width, height) = if orientation < 5 { (48, 32) } else { (32, 48) };
        assert_eq!(cleaned.dimensions(), (width, height));
        let positions = [
            (width / 4, height / 4),
            (3 * width / 4, height / 4),
            (width / 4, 3 * height / 4),
            (3 * width / 4, 3 * height / 4),
        ];
        for ((x, y), expected_index) in positions.into_iter().zip(layout) {
            let actual = cleaned.get_pixel(x, y).0;
            for (actual, expected) in actual.into_iter().zip(colors[expected_index]) {
                assert!(
                    actual.abs_diff(expected) <= 20,
                    "orientation {orientation} at ({x}, {y}): {actual} vs {expected}"
                );
            }
        }
    }
}

#[test]
fn competing_clean_commands_publish_exactly_one_copy() {
    let dir = tempdir().unwrap();
    let source = dir.path().join("input.jpg");
    let destination = dir.path().join("result.jpg");
    fs::write(&source, jpeg_with_comment()).unwrap();

    let codes = std::thread::scope(|scope| {
        let first = scope.spawn(|| metasafe("clean", &source, Some(&destination)).status.code());
        let second = scope.spawn(|| metasafe("clean", &source, Some(&destination)).status.code());
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert!(
        codes == [Some(0), Some(2)] || codes == [Some(2), Some(0)],
        "{codes:?}"
    );
    assert!(metasafe("verify", &destination, None).status.success());
}

#[test]
fn batch_continues_after_invalid_input_and_preserves_originals() {
    let dir = tempdir().unwrap();
    let output_dir = dir.path().join("Очищенные фото");
    fs::create_dir(&output_dir).unwrap();
    let invalid = dir.path().join("invalid.jpg");
    let source = dir.path().join("Первое фото.JPEG");
    let second = dir.path().join("second.jpg");
    let jpeg = jpeg_with_comment();
    fs::write(&invalid, b"invalid JPEG").unwrap();
    fs::write(&source, &jpeg).unwrap();
    fs::write(&second, &jpeg).unwrap();

    let result = batch(&[&invalid, &source, &second], &output_dir);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    for file in ["Первое фото_clean.jpg", "second_clean.jpg"] {
        let verification = metasafe("verify", &output_dir.join(file), None);
        assert!(verification.status.success(), "{verification:?}");
    }
    assert!(!output_dir.join("invalid_clean.jpg").exists());
    assert_eq!(fs::read(&source).unwrap(), jpeg);
    assert_eq!(fs::read(&second).unwrap(), jpeg);
    assert_eq!(fs::read(&invalid).unwrap(), b"invalid JPEG");
}

#[test]
fn batch_keeps_existing_files_and_the_first_copy_for_a_shared_stem() {
    let dir = tempdir().unwrap();
    let output_dir = dir.path().join("output");
    fs::create_dir(&output_dir).unwrap();
    let source = dir.path().join("shared.jpg");
    let same_stem = dir.path().join("shared.jpeg");
    let occupied = dir.path().join("occupied.jpeg");
    let reference = dir.path().join("reference.jpg");
    let existing = output_dir.join("occupied_clean.jpg");
    let first_jpeg = jpeg_with_comment();
    let mut other_jpeg = Vec::new();
    JpegEncoder::new(&mut other_jpeg)
        .encode_image(&RgbImage::from_pixel(11, 7, Rgb([200, 12, 60])))
        .unwrap();
    fs::write(&source, &first_jpeg).unwrap();
    fs::write(&same_stem, &other_jpeg).unwrap();
    fs::write(&occupied, &first_jpeg).unwrap();
    fs::write(&existing, b"existing output").unwrap();
    let expected = metasafe("clean", &source, Some(&reference));
    assert!(expected.status.success(), "{expected:?}");

    let result = batch(&[&occupied, &source, &same_stem, &source], &output_dir);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert_eq!(
        fs::read(output_dir.join("shared_clean.jpg")).unwrap(),
        fs::read(&reference).unwrap()
    );
    assert_eq!(fs::read(&existing).unwrap(), b"existing output");
    assert_eq!(fs::read(&source).unwrap(), first_jpeg);
    assert_eq!(fs::read(&same_stem).unwrap(), other_jpeg);
    assert_eq!(fs::read_dir(&output_dir).unwrap().count(), 2);
}

#[test]
fn batch_returns_success_only_when_every_copy_succeeds() {
    let dir = tempdir().unwrap();
    let output_dir = dir.path().join("output");
    let source = dir.path().join("photo.jpg");
    fs::create_dir(&output_dir).unwrap();
    fs::write(&source, jpeg_with_comment()).unwrap();
    let missing_directory = dir.path().join("missing");

    let invalid_directory = batch(&[&source], &missing_directory);
    assert_eq!(
        invalid_directory.status.code(),
        Some(2),
        "{invalid_directory:?}"
    );
    assert!(!missing_directory.exists());
    let success = batch(&[&source], &output_dir);
    assert!(success.status.success(), "{success:?}");
    let conflict = batch(&[&source], &output_dir);
    assert_eq!(conflict.status.code(), Some(1), "{conflict:?}");
}
