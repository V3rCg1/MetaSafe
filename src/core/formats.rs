use std::{
    io::{Cursor, Read},
    path::Path,
};

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};

use super::{
    Error, external,
    metadata::{self, MetadataKind as Kind, Report, ScanError, Status},
};

pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "webp", "gif", "tif", "tiff", "bmp", "dib", "ico",
    "tga", "qoi", "pnm", "pbm", "pgm", "ppm", "pam", "hdr", "dds", "heic", "heif", "hif", "avif",
    "psd", "psb", "jp2", "j2k", "jxl", "dng", "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2",
    "raf", "orf", "rw2", "pef", "srw", "3fr", "erf", "iiq", "kdc", "mrw", "rwl", "x3f",
];

const MAX_SIDE: u32 = 16_384;
const MAX_PIXELS: u64 = 40_000_000;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

pub(super) fn decoder(bytes: &[u8], format: ImageFormat) -> Result<impl ImageDecoder + '_, Error> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::PixelLimit(width, height));
    }
    Ok(decoder)
}

pub(super) fn decode_jpeg(bytes: &[u8]) -> Result<DynamicImage, Error> {
    Ok(DynamicImage::from_decoder(decoder(
        bytes,
        ImageFormat::Jpeg,
    )?)?)
}

pub(super) fn read_image(
    bytes: &[u8],
    source: &Path,
    pixels: bool,
) -> Result<(Report, Option<DynamicImage>), Error> {
    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let guessed = image::guess_format(bytes).ok();
    if let Some(format) = native_format(guessed, &extension) {
        if format == ImageFormat::Jpeg {
            let report = metadata::scan(bytes)?;
            let image = if pixels {
                Some(decode_jpeg(bytes)?)
            } else {
                None
            };
            return Ok((report, image));
        }
        let mut decoder = decoder(bytes, format)?;
        let mut report = match format {
            ImageFormat::Png => scan_png(bytes)?,
            ImageFormat::WebP => scan_webp(bytes)?,
            ImageFormat::Gif => scan_gif(bytes)?,
            _ => incomplete_report(format),
        };
        if !matches!(format, ImageFormat::Png | ImageFormat::WebP)
            && let Some(data) = decoder.exif_metadata()?
        {
            report.inspect_exif(data.strip_prefix(b"Exif\0\0").unwrap_or(&data));
        } else if format == ImageFormat::Tiff {
            report.inspect_exif(bytes);
        }
        if !matches!(
            format,
            ImageFormat::Png | ImageFormat::WebP | ImageFormat::Gif
        ) && let Some(data) = decoder.xmp_metadata()?
        {
            report.add_text(Kind::Xmp, "XMP", &data);
        }
        if let Some(data) = decoder.iptc_metadata()? {
            report.record(Kind::Iptc, Status::Present);
            report.inspect_iptc(&data);
        }
        if let Some(data) = decoder.icc_profile()? {
            report.record(Kind::Icc, Status::Present);
            report.add_detail(Kind::Icc, "Profile bytes", &data.len().to_string());
        }
        let image = if pixels {
            Some(DynamicImage::from_decoder(decoder)?)
        } else {
            None
        };
        return Ok((report, image));
    }

    let (format, coder) = external_format(&extension, bytes).ok_or(Error::UnsupportedFormat)?;
    external::read(bytes, &extension, format, coder, pixels)
}

fn native_format(guess: Option<ImageFormat>, extension: &str) -> Option<ImageFormat> {
    // Several camera RAW containers share the TIFF signature.
    if raw_extension(extension) {
        return None;
    }
    match guess {
        Some(
            format @ (ImageFormat::Jpeg
            | ImageFormat::Png
            | ImageFormat::WebP
            | ImageFormat::Gif
            | ImageFormat::Tiff
            | ImageFormat::Bmp
            | ImageFormat::Ico
            | ImageFormat::Pnm
            | ImageFormat::Tga
            | ImageFormat::Qoi
            | ImageFormat::Hdr
            | ImageFormat::Dds),
        ) => Some(format),
        None if extension == "tga" => Some(ImageFormat::Tga),
        _ => None,
    }
}

fn raw_extension(extension: &str) -> bool {
    matches!(
        extension,
        "dng"
            | "cr2"
            | "cr3"
            | "nef"
            | "nrw"
            | "arw"
            | "srf"
            | "sr2"
            | "raf"
            | "orf"
            | "rw2"
            | "pef"
            | "srw"
            | "3fr"
            | "erf"
            | "iiq"
            | "kdc"
            | "mrw"
            | "rwl"
            | "x3f"
    )
}

fn external_format(extension: &str, bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if raw_extension(extension) {
        return Some(("Camera RAW", "DNG"));
    }
    if bytes.starts_with(b"8BPS") {
        return Some(("Photoshop", "PSD"));
    }
    if bytes.starts_with(b"\0\0\0\x0cjP  \r\n\x87\n") || bytes.starts_with(b"\xff\x4f\xff\x51") {
        return Some(("JPEG 2000", "JP2"));
    }
    if bytes.starts_with(b"\xff\x0a") || bytes.starts_with(b"\0\0\0\x0cJXL \r\n\x87\n") {
        return Some(("JPEG XL", "JXL"));
    }
    if bytes.get(4..8) == Some(b"ftyp") {
        let size = bytes
            .get(..4)
            .and_then(|value| value.try_into().ok())
            .map(u32::from_be_bytes)? as usize;
        let brands = bytes.get(8..size.min(256))?;
        let mut compatible = brands
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != 1)
            .map(|(_, brand)| brand);
        if compatible
            .clone()
            .any(|brand| matches!(brand, b"avif" | b"avis"))
        {
            return Some(("AVIF", "AVIF"));
        }
        if compatible.any(|brand| {
            matches!(
                brand,
                b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
            )
        }) {
            return Some(("HEIF", "HEIC"));
        }
    }
    None
}

fn incomplete_report(format: ImageFormat) -> Report {
    let mut report = Report::new();
    report.format = format!("{format:?}").to_ascii_uppercase();
    report.single_frame_export = matches!(format, ImageFormat::Tiff | ImageFormat::Ico);
    for check in &mut report.checks {
        if !matches!(check.kind, Kind::Exif | Kind::Gps) {
            check.status = Status::Unverified;
        }
    }
    report
}

fn add_unknown(report: &mut Report, kind: &[u8], size: usize) {
    report.record(Kind::Unknown, Status::Present);
    report.add_detail(
        Kind::Unknown,
        &String::from_utf8_lossy(kind),
        &format!("{size} bytes"),
    );
}

fn trailing(report: &mut Report, bytes: &[u8]) {
    if !bytes.is_empty() {
        report.record(Kind::TrailingData, Status::Present);
        report.add_detail(Kind::TrailingData, "Bytes", &bytes.len().to_string());
    }
}

pub(super) fn scan_png(mut bytes: &[u8]) -> Result<Report, ScanError> {
    let mut report = Report::new();
    report.format = "PNG".into();
    bytes = bytes
        .strip_prefix(b"\x89PNG\r\n\x1a\n")
        .ok_or(ScanError::InvalidContainer)?;
    for _ in 0..65_536 {
        let header = bytes.get(..8).ok_or(ScanError::InvalidContainer)?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let kind = &header[4..];
        let end = length.checked_add(12).ok_or(ScanError::InvalidContainer)?;
        let data = bytes.get(8..end - 4).ok_or(ScanError::InvalidContainer)?;
        bytes = bytes.get(end..).ok_or(ScanError::InvalidContainer)?;
        match kind {
            b"IHDR" | b"PLTE" | b"IDAT" | b"tRNS" => {}
            b"acTL" => {
                report.single_frame_export = true;
                add_unknown(&mut report, kind, data.len());
            }
            b"IEND" if data.is_empty() => {
                trailing(&mut report, bytes);
                return Ok(report);
            }
            b"eXIf" => report.inspect_exif(data.strip_prefix(b"Exif\0\0").unwrap_or(data)),
            b"iCCP" => {
                report.record(Kind::Icc, Status::Present);
                if let Some((name, _)) = split_null(data) {
                    report.add_text(Kind::Icc, "Profile name", name);
                }
            }
            b"tEXt" | b"iTXt" | b"zTXt" => {
                if let Some((name, value)) = split_null(data) {
                    let group = if name == b"XML:com.adobe.xmp" {
                        Kind::Xmp
                    } else {
                        Kind::Comment
                    };
                    let name = String::from_utf8_lossy(&name[..name.len().min(79)]);
                    match kind {
                        b"tEXt" => report.add_text(group, &name, value),
                        b"iTXt" if value.starts_with(&[0, 0]) => {
                            let text = value[2..]
                                .splitn(3, |&byte| byte == 0)
                                .nth(2)
                                .unwrap_or_default();
                            report.add_text(group, &name, text);
                        }
                        b"iTXt" if value.starts_with(&[1, 0]) => {
                            let text = value[2..]
                                .splitn(3, |&byte| byte == 0)
                                .nth(2)
                                .unwrap_or_default();
                            add_compressed(&mut report, group, &name, text);
                        }
                        b"zTXt" if value.starts_with(&[0]) => {
                            add_compressed(&mut report, group, &name, &value[1..]);
                        }
                        _ => {
                            report.record(group, Status::Present);
                            report.add_detail(group, &name, "Unreadable text");
                        }
                    }
                } else {
                    add_unknown(&mut report, kind, data.len());
                }
            }
            _ => add_unknown(&mut report, kind, data.len()),
        }
    }
    Err(ScanError::InvalidContainer)
}

fn scan_webp(bytes: &[u8]) -> Result<Report, ScanError> {
    let mut report = Report::new();
    report.format = "WEBP".into();
    let header = bytes.get(..12).ok_or(ScanError::InvalidContainer)?;
    let declared = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let end = declared.checked_add(8).ok_or(ScanError::InvalidContainer)?;
    let mut chunks = bytes.get(12..end).ok_or(ScanError::InvalidContainer)?;
    trailing(&mut report, &bytes[end..]);
    for _ in 0..65_536 {
        if chunks.is_empty() {
            return Ok(report);
        }
        let header = chunks.get(..8).ok_or(ScanError::InvalidContainer)?;
        let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let end = 8_usize
            .checked_add(length)
            .ok_or(ScanError::InvalidContainer)?;
        let data = chunks.get(8..end).ok_or(ScanError::InvalidContainer)?;
        chunks = chunks
            .get(
                end.checked_add(length % 2)
                    .ok_or(ScanError::InvalidContainer)?..,
            )
            .ok_or(ScanError::InvalidContainer)?;
        match &header[..4] {
            b"VP8 " | b"VP8L" | b"VP8X" | b"ALPH" | b"ANMF" => {}
            b"ANIM" => report.single_frame_export = true,
            b"EXIF" => report.inspect_exif(data.strip_prefix(b"Exif\0\0").unwrap_or(data)),
            b"XMP " => report.add_text(Kind::Xmp, "XMP", data),
            b"ICCP" => {
                report.record(Kind::Icc, Status::Present);
                report.add_detail(Kind::Icc, "Profile bytes", &data.len().to_string());
            }
            kind => add_unknown(&mut report, kind, data.len()),
        }
    }
    Err(ScanError::InvalidContainer)
}

fn split_null(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let position = bytes.iter().position(|&byte| byte == 0)?;
    Some((&bytes[..position], &bytes[position + 1..]))
}

fn add_compressed(report: &mut Report, group: Kind, name: &str, compressed: &[u8]) {
    let mut text = Vec::new();
    // Only decode enough text to populate the disclosure.
    match flate2::read::ZlibDecoder::new(compressed)
        .take(513)
        .read_to_end(&mut text)
    {
        Ok(_) => report.add_text(group, name, &text),
        Err(_) => {
            report.record(group, Status::Present);
            report.add_detail(group, name, "Unreadable compressed text");
        }
    }
}

fn scan_gif(mut bytes: &[u8]) -> Result<Report, ScanError> {
    let mut report = Report::new();
    report.format = "GIF".into();
    let mut saw_frame = false;
    let header = bytes.get(..13).ok_or(ScanError::InvalidContainer)?;
    bytes = bytes
        .get(13 + gif_palette_bytes(header[10])..)
        .ok_or(ScanError::InvalidContainer)?;
    for _ in 0..65_536 {
        let (&marker, rest) = bytes.split_first().ok_or(ScanError::InvalidContainer)?;
        bytes = rest;
        match marker {
            0x3b => {
                trailing(&mut report, bytes);
                return Ok(report);
            }
            0x2c => {
                report.single_frame_export |= saw_frame;
                saw_frame = true;
                let image = bytes.get(..9).ok_or(ScanError::InvalidContainer)?;
                bytes = bytes
                    .get(10 + gif_palette_bytes(image[8])..)
                    .ok_or(ScanError::InvalidContainer)?;
                bytes = gif_blocks(bytes, None)?;
            }
            0x21 => {
                let (&label, rest) = bytes.split_first().ok_or(ScanError::InvalidContainer)?;
                let mut content = Vec::new();
                bytes = gif_blocks(rest, Some(&mut content))?;
                match label {
                    0xfe => report.add_text(Kind::Comment, "GIF comment", &content),
                    0xf9 => {}
                    0xff if content.starts_with(b"XMP DataXMP") => {
                        report.add_text(Kind::Xmp, "XMP", &content[11..]);
                    }
                    0xff if content.starts_with(b"NETSCAPE2.0")
                        || content.starts_with(b"ANIMEXTS1.0") => {}
                    _ => add_unknown(&mut report, &[label], content.len()),
                }
            }
            _ => return Err(ScanError::InvalidContainer),
        }
    }
    Err(ScanError::InvalidContainer)
}

fn gif_palette_bytes(flags: u8) -> usize {
    if flags & 0x80 == 0 {
        0
    } else {
        3 << (usize::from(flags & 7) + 1)
    }
}

fn gif_blocks<'a>(
    mut bytes: &'a [u8],
    mut prefix: Option<&mut Vec<u8>>,
) -> Result<&'a [u8], ScanError> {
    loop {
        let (&length, rest) = bytes.split_first().ok_or(ScanError::InvalidContainer)?;
        if length == 0 {
            return Ok(rest);
        }
        let length = usize::from(length);
        let data = rest.get(..length).ok_or(ScanError::InvalidContainer)?;
        if let Some(prefix) = prefix.as_deref_mut() {
            prefix.extend_from_slice(&data[..length.min(513_usize.saturating_sub(prefix.len()))]);
        }
        bytes = &rest[length..];
    }
}
