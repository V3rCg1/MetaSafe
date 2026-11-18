mod office;
mod pdf;

use std::path::Path;

use super::{
    Error,
    metadata::{Check, MetadataKind, Report, Status},
};

pub const EXTENSIONS: &[&str] = &["pdf", "docx", "xlsx"];

pub fn is_document(path: &Path) -> bool {
    extension(path).is_some()
}

pub(super) fn extension(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?;
    EXTENSIONS
        .iter()
        .copied()
        .find(|format| extension.eq_ignore_ascii_case(format))
}

pub(super) fn inspect(bytes: &[u8], format: &str) -> Result<Report, Error> {
    match format {
        "pdf" => pdf::inspect(bytes),
        "docx" => office::inspect(bytes, "DOCX"),
        "xlsx" => office::inspect(bytes, "XLSX"),
        _ => Err(unsupported("unknown document format")),
    }
}

pub(super) fn clean(bytes: &[u8], format: &str) -> Result<Vec<u8>, Error> {
    match format {
        "pdf" => pdf::clean(bytes),
        "docx" => office::clean(bytes, "DOCX"),
        "xlsx" => office::clean(bytes, "XLSX"),
        _ => Err(unsupported("unknown document format")),
    }
}

pub(super) fn unsupported(detail: &str) -> Error {
    Error::Document(detail.to_owned())
}

fn report(format: &str, kinds: &[MetadataKind]) -> Report {
    let mut report = Report::new();
    report.format = format.to_owned();
    report.checks = kinds
        .iter()
        .map(|&kind| Check {
            kind,
            status: Status::NotDetected,
        })
        .collect();
    report
}
