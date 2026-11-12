use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use image::{DynamicImage, ImageFormat};
use serde_json::Value;
use wait_timeout::ChildExt;

use super::{
    Error, MAX_FILE_BYTES,
    formats::decoder,
    metadata::{MetadataKind as Kind, Report, Status},
};

const MAX_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RAW_RENDER_BYTES: u64 = 40_000_000 * 6 + 4096;

pub(super) fn read(
    bytes: &[u8],
    extension: &str,
    format: &str,
    coder: &str,
    pixels: bool,
) -> Result<(Report, Option<DynamicImage>), Error> {
    let directory = tempfile::Builder::new();
    #[cfg(unix)]
    let directory = {
        use std::os::unix::fs::PermissionsExt;
        let mut directory = directory;
        directory.permissions(fs::Permissions::from_mode(0o700));
        directory
    };
    let directory = directory.tempdir().map_err(io_error)?;
    let safe_extension = match coder {
        "DNG" => extension,
        "HEIC" => "heic",
        "AVIF" => "avif",
        "PSD" => "psd",
        "JP2" => "jp2",
        "JXL" => "jxl",
        _ => return Err(Error::UnsupportedFormat),
    };
    let input = directory.path().join(format!("image.{safe_extension}"));
    fs::write(&input, bytes).map_err(io_error)?;

    let mut report = Report::new();
    report.format = format.into();
    report.single_frame_export = true;
    for check in &mut report.checks {
        check.status = Status::Unverified;
    }
    read_tags(&input, &mut report)?;
    let image = if pixels && coder == "DNG" {
        let mut converter = command("dcraw_emu");
        converter
            .current_dir(directory.path())
            .env("OMP_NUM_THREADS", "2")
            .args(["-w", "-q", "2", "-6", "-Z", "-"])
            .arg(&input);
        let ppm = output(converter, "dcraw_emu", MAX_RAW_RENDER_BYTES)?;
        let decoder = decoder(&ppm, ImageFormat::Pnm)?;
        Some(DynamicImage::from_decoder(decoder)?)
    } else if pixels {
        let mut converter = command("magick");
        let input = format!("{coder}:image.{safe_extension}[0]");
        converter
            .current_dir(directory.path())
            .env("MAGICK_TEMPORARY_PATH", directory.path())
            .args([
                "-limit", "width", "16384", "-limit", "height", "16384", "-limit", "area", "40MP",
                "-limit", "memory", "256MiB", "-limit", "map", "256MiB", "-limit", "disk",
                "512MiB", "-limit", "thread", "2", "-limit", "time", "20",
            ])
            .arg(input)
            .args(["-auto-orient", "-strip", "-depth", "8", "PNG32:-"]);
        let png = output(converter, "magick", MAX_FILE_BYTES)?;
        let decoder = decoder(&png, ImageFormat::Png)?;
        Some(DynamicImage::from_decoder(decoder)?)
    } else {
        None
    };
    Ok((report, image))
}

fn read_tags(input: &Path, report: &mut Report) -> Result<(), Error> {
    let mut exiftool = command("exiftool");
    exiftool.args(["-j", "-G1", "-s", "-e"]).arg(input);
    let json = output(exiftool, "exiftool", MAX_METADATA_BYTES)?;
    let tags: Vec<serde_json::Map<String, Value>> = serde_json::from_slice(&json)
        .map_err(|error| Error::ExternalTool(format!("ExifTool: {error}")))?;
    let tags = tags.first().ok_or(Error::UnsupportedFormat)?;
    if !tags.contains_key("File:FileType") {
        return Err(Error::UnsupportedFormat);
    }
    for (name, value) in tags {
        let Some((group, label)) = name.split_once(':') else {
            continue;
        };
        let kind = match group {
            "ExifTool" | "System" | "Composite" => continue,
            "File" if label == "Comment" => Kind::Comment,
            "File" => continue,
            "GPS" => Kind::Gps,
            "ExifIFD" | "IFD0" | "IFD1" | "InteropIFD" | "SubIFD" => Kind::Exif,
            "IPTC" => Kind::Iptc,
            group if group.starts_with("XMP") => Kind::Xmp,
            group if group.starts_with("ICC") => Kind::Icc,
            _ => Kind::Unknown,
        };
        report.record(kind, Status::Present);
        let value = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        report.add_detail(kind, label, &value);
    }
    Ok(())
}

fn command(name: &str) -> Command {
    let variable = format!("METASAFE_{}", name.to_ascii_uppercase());
    if let Some(path) = std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Command::new(path);
    }
    #[cfg(target_os = "macos")]
    for base in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let path = Path::new(base).join(name);
        if path.is_file() {
            return Command::new(path);
        }
    }
    Command::new(name)
}

fn output(mut command: Command, name: &'static str, limit: u64) -> Result<Vec<u8>, Error> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                Error::MissingTool(name)
            } else {
                io_error(error)
            }
        })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let (send, receive) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = send.send(result);
    });
    let status = match child.wait_timeout(Duration::from_secs(30)) {
        Ok(Some(status)) => status,
        result => {
            let _ = child.kill();
            let _ = child.wait();
            // A delegate may still hold the pipe. Do not wait for its reader here.
            return Err(Error::ExternalTool(match result {
                Err(error) => format!("{name}: {error}"),
                _ => format!("{name}: processing timed out"),
            }));
        }
    };
    let bytes = receive
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| Error::ExternalTool(format!("{name}: output pipe did not close")))?
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(Error::ExternalTool(format!(
            "{name}: output exceeds the size limit"
        )));
    }
    if !status.success() {
        return Err(Error::ExternalTool(format!(
            "{name}: format could not be processed ({status})"
        )));
    }
    Ok(bytes)
}

fn io_error(error: io::Error) -> Error {
    Error::ExternalTool(error.to_string())
}
