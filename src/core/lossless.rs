use image::ImageFormat;

use super::{
    Error,
    metadata::{ScanError, is_plain_jfif, scan_jpeg_segments},
};

const CHUNK_LIMIT: usize = 65_536;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

pub(super) fn strip(bytes: &[u8], format: ImageFormat) -> Result<Vec<u8>, Error> {
    match format {
        ImageFormat::Jpeg => strip_jpeg(bytes),
        ImageFormat::Png => strip_png(bytes),
        ImageFormat::WebP => strip_webp(bytes),
        _ => Err(Error::UnsupportedLosslessFormat),
    }
}

fn strip_jpeg(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut copy = b"\xff\xd8".to_vec();
    let mut kept_jfif = false;
    let mut adobe_transform = false;
    scan_jpeg_segments(bytes, |marker, raw, data| match marker {
        Some(0xe0) if !kept_jfif && is_plain_jfif(data) => {
            kept_jfif = true;
            copy.extend_from_slice(raw);
        }
        Some(0xee) if data.starts_with(b"Adobe") => adobe_transform = true,
        Some(0xe0..=0xef | 0xfe) => {}
        _ => copy.extend_from_slice(raw),
    })?;
    if adobe_transform {
        return Err(Error::LosslessTransformRequired);
    }
    Ok(copy)
}

fn strip_png(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut remaining = bytes
        .strip_prefix(PNG_SIGNATURE)
        .ok_or(ScanError::InvalidContainer)?;
    let mut copy = PNG_SIGNATURE.to_vec();
    for _ in 0..CHUNK_LIMIT {
        let header = remaining.get(..8).ok_or(ScanError::InvalidContainer)?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let end = length.checked_add(12).ok_or(ScanError::InvalidContainer)?;
        let chunk = remaining.get(..end).ok_or(ScanError::InvalidContainer)?;
        remaining = &remaining[end..];
        let kind = &header[4..];
        let crc = u32::from_be_bytes(chunk[end - 4..].try_into().unwrap());
        if crc32fast::hash(&chunk[4..end - 4]) != crc {
            return Err(ScanError::InvalidContainer.into());
        }
        match kind {
            b"IHDR" | b"PLTE" | b"IDAT" | b"tRNS" => copy.extend_from_slice(chunk),
            b"IEND" if length == 0 => {
                copy.extend_from_slice(chunk);
                return Ok(copy);
            }
            b"acTL" | b"fcTL" | b"fdAT" => return Err(Error::UnsupportedLosslessFormat),
            _ if kind[0].is_ascii_lowercase() => {}
            _ => return Err(Error::UnsupportedLosslessFormat),
        }
    }
    Err(ScanError::InvalidContainer.into())
}

fn strip_webp(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let header = bytes.get(..12).ok_or(ScanError::InvalidContainer)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WEBP" {
        return Err(ScanError::InvalidContainer.into());
    }
    let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let end = length.checked_add(8).ok_or(ScanError::InvalidContainer)?;
    let mut remaining = bytes.get(12..end).ok_or(ScanError::InvalidContainer)?;
    let mut copy = header.to_vec();
    for _ in 0..CHUNK_LIMIT {
        if remaining.is_empty() {
            let length = u32::try_from(copy.len() - 8).map_err(|_| ScanError::InvalidContainer)?;
            copy[4..8].copy_from_slice(&length.to_le_bytes());
            return Ok(copy);
        }
        let header = remaining.get(..8).ok_or(ScanError::InvalidContainer)?;
        let length = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
        let end = length
            .checked_add(8 + length % 2)
            .ok_or(ScanError::InvalidContainer)?;
        let chunk = remaining.get(..end).ok_or(ScanError::InvalidContainer)?;
        remaining = &remaining[end..];
        match &header[..4] {
            b"VP8 " | b"VP8L" | b"ALPH" => {
                copy.extend_from_slice(&chunk[..8 + length]);
                if !length.is_multiple_of(2) {
                    copy.push(0);
                }
            }
            b"VP8X" if length == 10 => {
                let start = copy.len();
                copy.extend_from_slice(chunk);
                if copy[start + 8] & 0x02 != 0 {
                    return Err(Error::UnsupportedLosslessFormat);
                }
                // Retain alpha and clear ICCP, EXIF and XMP flags.
                copy[start + 8] &= 0x10;
                copy[start + 9..start + 12].fill(0);
            }
            b"VP8X" => return Err(ScanError::InvalidContainer.into()),
            b"ANIM" | b"ANMF" => return Err(Error::UnsupportedLosslessFormat),
            _ => {}
        }
    }
    Err(ScanError::InvalidContainer.into())
}
