use std::path::{Path, PathBuf};

use super::{
    CleanMode, Error, clean_with_options, metadata::Report, suggested_output_extension_for_mode,
};

#[derive(Debug)]
pub struct BatchItem {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub result: Result<Report, Error>,
}

#[derive(Debug)]
pub struct BatchReport {
    pub items: Vec<BatchItem>,
}

/// Clean files in selection order, publishing each verified copy independently.
///
/// Existing destinations are reported as per-file errors. Progress receives
/// each result once, including failures, after that file finishes.
pub fn batch_clean<I, P, F>(
    sources: I,
    output_dir: &Path,
    progress: F,
) -> Result<BatchReport, Error>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
    F: FnMut(&BatchItem),
{
    batch_clean_with_mode(sources, output_dir, CleanMode::Reencode, progress)
}

pub fn batch_clean_with_mode<I, P, F>(
    sources: I,
    output_dir: &Path,
    mode: CleanMode,
    progress: F,
) -> Result<BatchReport, Error>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
    F: FnMut(&BatchItem),
{
    batch_clean_with_options(sources, output_dir, mode, false, progress)
}

pub fn batch_clean_with_options<I, P, F>(
    sources: I,
    output_dir: &Path,
    mode: CleanMode,
    strip_h264_sei: bool,
    mut progress: F,
) -> Result<BatchReport, Error>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
    F: FnMut(&BatchItem),
{
    let metadata = output_dir
        .metadata()
        .map_err(|source| Error::OutputDirectory {
            path: output_dir.to_owned(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(Error::NotADirectory(output_dir.to_owned()));
    }

    let mut items = Vec::new();
    for source in sources {
        let source = source.as_ref();
        let item = match source.file_stem() {
            Some(stem) if valid_output_name(stem) => {
                let mut name = stem.to_owned();
                name.push("_clean.");
                name.push(suggested_output_extension_for_mode(source, mode));
                let destination = output_dir.join(name);
                BatchItem {
                    source: source.to_owned(),
                    result: clean_with_options(source, &destination, mode, strip_h264_sei),
                    destination,
                }
            }
            _ => BatchItem {
                source: source.to_owned(),
                destination: output_dir.to_owned(),
                result: Err(Error::InvalidSourceName(source.to_owned())),
            },
        };
        progress(&item);
        items.push(item);
    }
    Ok(BatchReport { items })
}

#[cfg(windows)]
pub(super) fn valid_output_name(name: &std::ffi::OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    // A colon could select a drive or an NTFS stream instead of creating a
    // regular file inside the output directory.
    !name.encode_wide().any(|unit| unit == u16::from(b':'))
}

#[cfg(not(windows))]
pub(super) fn valid_output_name(_: &std::ffi::OsStr) -> bool {
    true
}
