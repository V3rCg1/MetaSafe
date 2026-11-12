mod batch;
mod cleaner;
mod external;
mod formats;
pub mod metadata;

use std::{
    fs::{File, Metadata, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
};

pub use batch::{BatchItem, BatchReport, batch_clean};
pub use cleaner::{clean, preview, suggested_output_extension};
pub use formats::EXTENSIONS;
use metadata::Report;

pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read {path:?}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("expected a regular file: {0:?}")]
    NotAFile(PathBuf),
    #[error("file exceeds the {MAX_FILE_BYTES}-byte limit: {0:?}")]
    FileTooLarge(PathBuf),
    #[error(transparent)]
    Scan(#[from] metadata::ScanError),
    #[error("EXIF orientation could not be verified; the copy was not created")]
    UnverifiedOrientation,
    #[error("unsupported image format")]
    UnsupportedFormat,
    #[error("install {0} to process this image format")]
    MissingTool(&'static str),
    #[error("image tool failed: {0}")]
    ExternalTool(String),
    #[error("use .png, .jpg or .jpeg for the cleaned copy")]
    UnsupportedOutputFormat,
    #[error("JPEG cannot preserve transparency; save a PNG copy")]
    TransparentJpeg,
    #[error("image dimensions {0} × {1} exceed the pixel limit")]
    PixelLimit(u32, u32),
    #[error("could not process image pixels: {0}")]
    Image(#[from] image::ImageError),
    #[error("output already exists: {0:?}")]
    OutputExists(PathBuf),
    #[error("could not write {path:?}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the new copy did not pass metadata verification")]
    VerificationFailed,
    #[error("published file differs from the verified copy: {0:?}")]
    PublishedOutputChanged(PathBuf),
    #[error("could not access output directory {path:?}: {source}")]
    OutputDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("expected an output directory: {0:?}")]
    NotADirectory(PathBuf),
    #[error("could not determine a valid output name from {0:?}")]
    InvalidSourceName(PathBuf),
    #[error("expected an output path with a regular file name: {0:?}")]
    InvalidDestinationName(PathBuf),
}

pub fn inspect(path: &Path) -> Result<Report, Error> {
    Ok(formats::read_image(&read_file(path)?, path, false)?.0)
}

pub fn verify(path: &Path) -> Result<Report, Error> {
    let bytes = read_file(path)?;
    Ok(formats::read_image(&bytes, path, true)?.0)
}

fn read_file(path: &Path) -> Result<Vec<u8>, Error> {
    let (file, metadata) = open_regular_file(path)?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(Error::FileTooLarge(path.to_path_buf()));
    }

    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| Error::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::FileTooLarge(path.to_path_buf()));
    }
    Ok(bytes)
}

fn open_regular_file(path: &Path) -> Result<(File, Metadata), Error> {
    let read_error = |source| Error::Read {
        path: path.to_path_buf(),
        source,
    };
    // Skip known pipes and devices before opening, then check the open handle.
    if !path.metadata().map_err(read_error)?.is_file() {
        return Err(Error::NotAFile(path.to_path_buf()));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // A concurrent rename could replace the file with a FIFO after the first check.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(read_error)?;
    let metadata = file.metadata().map_err(read_error)?;
    if !metadata.is_file() {
        return Err(Error::NotAFile(path.to_path_buf()));
    }
    Ok((file, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_rejects_jpeg_that_cannot_be_decoded() {
        let image = image::GrayImage::from_pixel(8, 8, image::Luma([64]));
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut bytes)
            .encode_image(&image)
            .unwrap();
        let scan = bytes
            .windows(2)
            .position(|marker| marker == [0xff, 0xda])
            .unwrap();
        // Select a Huffman table that the encoder did not emit.
        bytes[scan + 6] = 0x33;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("invalid-image.jpg");
        std::fs::write(&path, bytes).unwrap();

        assert!(inspect(&path).unwrap().passed());
        assert!(matches!(verify(&path), Err(Error::Image(_))));
    }
}
