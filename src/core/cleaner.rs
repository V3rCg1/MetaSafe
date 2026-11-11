use std::{
    io::{self, BufWriter, Cursor, Read, Seek, Write},
    path::Path,
};

use image::{
    DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, codecs::jpeg::JpegEncoder,
    metadata::Orientation,
};
use tempfile::NamedTempFile;

use super::{
    Error, MAX_FILE_BYTES,
    batch::valid_output_name,
    metadata::{self, Report},
    open_regular_file, read_file,
};

const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 40_000_000;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const JPEG_QUALITY: u8 = 90;
const PREVIEW_SIDE: u32 = 1_280;

pub fn clean(source: &Path, destination: &Path) -> Result<Report, Error> {
    if !destination.file_name().is_some_and(valid_output_name) {
        return Err(Error::InvalidDestinationName(destination.to_owned()));
    }
    let bytes = read_file(source)?;
    let report = metadata::scan(&bytes)?;
    if report.orientation_unverified() {
        return Err(Error::UnverifiedOrientation);
    }
    let mut image = decode_jpeg(&bytes)?;
    // Apply the same orientation reported by the EXIF scanner to the decoded pixels.
    image.apply_orientation(orientation(&report));
    drop(bytes);

    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let write_error = |source: io::Error| match source.kind() {
        io::ErrorKind::AlreadyExists => Error::OutputExists(destination.to_path_buf()),
        _ => Error::Write {
            path: destination.to_path_buf(),
            source,
        },
    };

    let mut temporary = NamedTempFile::new_in(parent).map_err(write_error)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        JpegEncoder::new_with_quality(&mut writer, JPEG_QUALITY)
            .encode_image(&image)
            .map_err(|error| match error {
                image::ImageError::IoError(source) => write_error(source),
                error => Error::Image(error),
            })?;
        writer.flush().map_err(write_error)?;
    }
    drop(image);
    let file = temporary.as_file_mut();
    file.rewind().map_err(write_error)?;
    let mut encoded = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut encoded)
        .map_err(write_error)?;
    if encoded.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::FileTooLarge(destination.to_path_buf()));
    }
    let report = metadata::scan(&encoded)?;
    if !report.passed() {
        return Err(Error::VerificationFailed);
    }
    decode_jpeg(&encoded)?;
    temporary.as_file().sync_all().map_err(write_error)?;
    // The no-clobber publication step owns the destination existence check.
    temporary
        .persist_noclobber(destination)
        .map_err(|error| write_error(error.error))?;
    // Persistence is path-based. Check what is visible at the final path, and
    // leave it alone on error because another process could own that entry.
    verify_published(destination, &encoded)?;
    Ok(report)
}

fn verify_published(path: &Path, expected: &[u8]) -> Result<(), Error> {
    let (mut file, metadata) = open_regular_file(path)?;
    let mismatch = || Error::PublishedOutputChanged(path.to_path_buf());
    let read_error = |source| Error::Read {
        path: path.to_path_buf(),
        source,
    };
    if metadata.len() != expected.len() as u64 {
        return Err(mismatch());
    }

    let mut buffer = [0; 8192];
    for chunk in expected.chunks(buffer.len()) {
        let actual = &mut buffer[..chunk.len()];
        match file.read_exact(actual) {
            Ok(()) if actual == chunk => (),
            Ok(()) => return Err(mismatch()),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Err(mismatch()),
            Err(error) => return Err(read_error(error)),
        }
    }
    match file.read_exact(&mut buffer[..1]) {
        Ok(()) => Err(mismatch()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(()),
        Err(error) => Err(read_error(error)),
    }
}

/// Inspect the file and return a preview bounded to 1,280 pixels per side.
pub fn preview(source: &Path) -> Result<(Report, image::RgbaImage), Error> {
    let bytes = read_file(source)?;
    let report = metadata::scan(&bytes)?;
    let mut image = decode_jpeg(&bytes)?;
    drop(bytes);
    if image.width() > PREVIEW_SIDE || image.height() > PREVIEW_SIDE {
        image = image.thumbnail(PREVIEW_SIDE, PREVIEW_SIDE);
    }
    image.apply_orientation(orientation(&report));
    Ok((report, image.into_rgba8()))
}

fn orientation(report: &Report) -> Orientation {
    report
        .orientation
        .and_then(|value| u8::try_from(value).ok())
        .and_then(Orientation::from_exif)
        .unwrap_or(Orientation::NoTransforms)
}

pub(super) fn decode_jpeg(bytes: &[u8]) -> Result<DynamicImage, Error> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(image::ImageError::IoError)?;
    if reader.format() != Some(ImageFormat::Jpeg) {
        return Err(Error::NotJpeg);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);

    let decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::PixelLimit(width, height));
    }
    Ok(DynamicImage::from_decoder(decoder)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_file_must_match_verified_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("copy.jpg");
        let mut contents = vec![1; 16_389];
        std::fs::write(&path, &contents).unwrap();
        assert!(verify_published(&path, &contents).is_ok());

        contents[8193] = 2;
        assert!(matches!(
            verify_published(&path, &contents),
            Err(Error::PublishedOutputChanged(_))
        ));
        assert!(matches!(
            verify_published(&path, &contents[..3]),
            Err(Error::PublishedOutputChanged(_))
        ));
        assert_eq!(std::fs::read(path).unwrap(), vec![1; contents.len()]);
    }
}
