use std::fmt::{self, Write as _};

use exif::{Context, In, Reader, Tag};
use thiserror::Error;

const MAX_MARKERS: usize = 65_536;
const MAX_EXIF_FIELDS: usize = 128;
const MAX_EXIF_VALUE_BYTES: usize = 512;
const MAX_EXIF_PARSE_BYTES: usize = 1024 * 1024;
const MAX_EXIF_PARSE_FIELDS: usize = 1024;
const MAX_EXIF_DIRECTORIES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataKind {
    Exif,
    Gps,
    Xmp,
    Iptc,
    Comment,
    Icc,
    Unknown,
    TrailingData,
}

impl fmt::Display for MetadataKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Exif => "EXIF",
            Self::Gps => "GPS",
            Self::Xmp => "XMP",
            Self::Iptc => "IPTC",
            Self::Comment => "JPEG comments",
            Self::Icc => "ICC profile",
            Self::Unknown => "Unclassified JPEG segments",
            Self::TrailingData => "Data after JPEG end marker",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    NotDetected,
    Present,
    Unverified,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotDetected => "not detected",
            Self::Present => "detected",
            Self::Unverified => "could not verify",
        })
    }
}

#[derive(Debug)]
pub struct Check {
    pub kind: MetadataKind,
    pub status: Status,
}

#[derive(Debug)]
pub struct ExifField {
    pub name: String,
    pub value: String,
    pub ifd: u16,
}

#[derive(Debug)]
pub struct MetadataDetail {
    pub kind: MetadataKind,
    pub name: String,
    pub value: String,
}

#[derive(Debug)]
pub struct Report {
    pub format: String,
    pub single_frame_export: bool,
    pub details: Vec<MetadataDetail>,
    pub checks: Vec<Check>,
    pub exif_fields: Vec<ExifField>,
    pub orientation: Option<u16>,
    pub exif_fields_truncated: bool,
    orientation_unverified: bool,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status == Status::NotDetected)
    }

    pub fn orientation_unverified(&self) -> bool {
        self.orientation_unverified
    }

    pub(super) fn new() -> Self {
        use MetadataKind::*;
        Self {
            format: "JPEG".into(),
            single_frame_export: false,
            details: Vec::new(),
            checks: [Exif, Gps, Xmp, Iptc, Comment, Icc, Unknown, TrailingData]
                .into_iter()
                .map(|kind| Check {
                    kind,
                    status: Status::NotDetected,
                })
                .collect(),
            exif_fields: Vec::new(),
            orientation: None,
            exif_fields_truncated: false,
            orientation_unverified: false,
        }
    }

    fn status(&self, kind: MetadataKind) -> Status {
        self.checks
            .iter()
            .find(|check| check.kind == kind)
            .map_or(Status::Unverified, |check| check.status)
    }

    pub(super) fn record(&mut self, kind: MetadataKind, status: Status) {
        if let Some(check) = self.checks.iter_mut().find(|check| check.kind == kind) {
            // Preserve a confirmed detection even if other segments cannot be read.
            if check.status != Status::Present {
                check.status = status;
            }
        }
    }

    pub(super) fn add_detail(&mut self, kind: MetadataKind, name: &str, value: &str) {
        if self.details.len() < 192 {
            let mut bounded = BoundedValue(String::new());
            if bounded.write_str(value).is_err() {
                bounded.0.push('…');
            }
            self.details.push(MetadataDetail {
                kind,
                name: name.into(),
                value: bounded.0,
            });
        }
    }

    pub(super) fn add_text(&mut self, kind: MetadataKind, name: &str, bytes: &[u8]) {
        self.record(kind, Status::Present);
        let prefix = &bytes[..bytes.len().min(MAX_EXIF_VALUE_BYTES)];
        let mut value = String::from_utf8_lossy(prefix).into_owned();
        if prefix.len() < bytes.len() {
            value.push('…');
        }
        self.add_detail(kind, name, &value);
    }

    pub(super) fn inspect_iptc(&mut self, mut data: &[u8]) {
        let mut found = false;
        while let Some(header) = data.get(..5).filter(|header| header[0] == 0x1c) {
            let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
            if length & 0x8000 != 0 {
                break;
            }
            let Some(value) = data.get(5..5 + length) else {
                break;
            };
            let label = match (header[1], header[2]) {
                (2, 5) => "Object name",
                (2, 25) => "Keywords",
                (2, 55) => "Date created",
                (2, 80) => "By-line",
                (2, 90) => "City",
                (2, 95) => "Province or state",
                (2, 101) => "Country",
                (2, 105) => "Headline",
                (2, 116) => "Copyright",
                (2, 120) => "Caption",
                _ => "",
            };
            let tag = format!("IPTC {}:{}", header[1], header[2]);
            self.add_text(
                MetadataKind::Iptc,
                if label.is_empty() { &tag } else { label },
                value,
            );
            data = &data[5 + length..];
            found = true;
        }
        if !found || !data.is_empty() {
            self.add_detail(
                MetadataKind::Iptc,
                "IPTC data",
                &format!("{} additional bytes", data.len()),
            );
        }
    }

    pub(super) fn inspect_exif(&mut self, payload: &[u8]) {
        if self.status(MetadataKind::Exif) != Status::NotDetected {
            self.orientation = None;
            self.orientation_unverified = true;
            self.record(MetadataKind::Gps, Status::Unverified);
            return;
        }

        // The JPEG walker has isolated this APP1 segment. The exif crate expects
        // the TIFF bytes without the APP1 identifier in read_raw.
        // https://docs.rs/kamadak-exif/0.6.1/exif/struct.Reader.html#method.read_raw
        let parsed = exif_fits_budget(payload, MAX_EXIF_PARSE_BYTES)
            .and_then(|()| Reader::new().read_raw(payload.to_vec()).ok());
        let Some(exif) = parsed else {
            self.orientation_unverified = true;
            self.record(MetadataKind::Exif, Status::Unverified);
            self.record(MetadataKind::Gps, Status::Unverified);
            return;
        };

        self.record(MetadataKind::Exif, Status::Present);
        if exif
            .fields()
            .any(|field| field.tag.context() == Context::Gps)
        {
            self.record(MetadataKind::Gps, Status::Present);
        }
        let mut orientations = exif
            .fields()
            .filter(|field| field.ifd_num == In::PRIMARY && field.tag == Tag::Orientation);
        if let Some(field) = orientations.next() {
            match &field.value {
                exif::Value::Short(values)
                    if values.len() == 1
                        && (1..=8).contains(&values[0])
                        && orientations.next().is_none() =>
                {
                    self.orientation = Some(values[0]);
                }
                _ => self.orientation_unverified = true,
            }
        }

        for field in exif.fields().take(MAX_EXIF_FIELDS) {
            let kind = if field.tag.context() == Context::Gps {
                MetadataKind::Gps
            } else {
                MetadataKind::Exif
            };
            let mut value = BoundedValue(String::new());
            let _ = write!(value, "{}", field.display_value());
            self.add_detail(kind, &field.tag.to_string(), &value.0);
        }
        self.exif_fields_truncated = exif.fields().count() > MAX_EXIF_FIELDS;
        self.exif_fields = exif
            .fields()
            .take(MAX_EXIF_FIELDS)
            .map(|field| {
                let mut display = BoundedValue(String::new());
                if write!(display, "{}", field.display_value()).is_err() {
                    display.0.push('…');
                }
                ExifField {
                    name: field.tag.to_string(),
                    value: display.0,
                    ifd: field.ifd_num.index(),
                }
            })
            .collect();
    }
}

type TiffByteOrder = (fn([u8; 2]) -> u16, fn([u8; 4]) -> u32);

// kamadak-exif caches each lazily decoded field. Count each referenced
// value separately even when multiple TIFF entries share the same bytes.
fn exif_fits_budget(data: &[u8], value_bytes: usize) -> Option<()> {
    let (short, long): TiffByteOrder = match data.get(..4)? {
        b"II\x2a\0" => (u16::from_le_bytes, u32::from_le_bytes),
        b"MM\0\x2a" => (u16::from_be_bytes, u32::from_be_bytes),
        _ => return None,
    };
    let mut budget = ExifBudget {
        data,
        short,
        long,
        value_bytes,
        fields: MAX_EXIF_PARSE_FIELDS,
        visited: Vec::new(),
    };
    let mut offset = long(data.get(4..8)?.try_into().ok()?) as usize;
    while offset != 0 {
        offset = budget.visit(offset, Context::Tiff)?;
    }
    Some(())
}

struct ExifBudget<'a> {
    data: &'a [u8],
    short: fn([u8; 2]) -> u16,
    long: fn([u8; 4]) -> u32,
    value_bytes: usize,
    fields: usize,
    visited: Vec<usize>,
}

impl ExifBudget<'_> {
    fn visit(&mut self, offset: usize, context: Context) -> Option<usize> {
        if self.visited.len() == MAX_EXIF_DIRECTORIES || self.visited.contains(&offset) {
            return None;
        }
        self.visited.push(offset);
        let directory = self.data.get(offset..)?;
        let count = usize::from((self.short)(directory.get(..2)?.try_into().ok()?));
        self.fields = self.fields.checked_sub(count)?;
        let table_end = 2 + count * 12;
        for entry in directory.get(2..table_end)?.as_chunks::<12>().0 {
            let tag = Tag(context, (self.short)(entry[..2].try_into().ok()?));
            let kind = (self.short)(entry[2..4].try_into().ok()?);
            let count = (self.long)(entry[4..8].try_into().ok()?) as usize;
            let encoded = entry[8..12].try_into().ok()?;
            let child = match tag {
                Tag::ExifIFDPointer => Some(Context::Exif),
                Tag::GPSInfoIFDPointer => Some(Context::Gps),
                Tag::InteropIFDPointer => Some(Context::Interop),
                _ => None,
            };
            if let Some(child) = child {
                if kind != 4 || count != 1 {
                    return None;
                }
                let child_offset = (self.long)(encoded) as usize;
                if child_offset == 0 || self.visit(child_offset, child)? != 0 {
                    return None;
                }
                continue;
            }
            self.charge(kind, count, encoded)?;
        }
        Some((self.long)(directory.get(table_end..table_end + 4)?.try_into().ok()?) as usize)
    }

    fn charge(&mut self, kind: u16, count: usize, encoded: [u8; 4]) -> Option<()> {
        let unit = match kind {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            // The decoder leaves unrecognized types opaque.
            _ => 0,
        };
        let size = count.checked_mul(unit)?;
        self.value_bytes = self.value_bytes.checked_sub(size)?;
        if size > 4 || kind == 2 {
            let value = if size <= 4 {
                &encoded[..size]
            } else {
                let offset = (self.long)(encoded) as usize;
                self.data.get(offset..)?.get(..size)?
            };
            if kind == 2 {
                // ASCII is decoded to a vector of separately owned strings.
                // Charge their headers, capacity growth and allocation overhead.
                let strings = value.iter().filter(|&&byte| byte == 0).count() + 1;
                self.value_bytes = self.value_bytes.checked_sub(strings * 96)?;
            }
        }
        Some(())
    }
}

struct BoundedValue(String);

impl fmt::Write for BoundedValue {
    fn write_str(&mut self, input: &str) -> fmt::Result {
        for character in input.chars() {
            if self.0.len() + character.len_utf8() > MAX_EXIF_VALUE_BYTES {
                return Err(fmt::Error);
            }
            self.0.push(character);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("input is not a JPEG image")]
    NotJpeg,
    #[error("invalid or truncated JPEG marker stream")]
    InvalidJpeg,
    #[error("invalid or truncated image container")]
    InvalidContainer,
    #[error("JPEG has too many markers")]
    MarkerLimit,
}

pub fn scan(input: &[u8]) -> Result<Report, ScanError> {
    scan_jpeg_segments(input, |_, _, _| {})
}

pub(super) fn scan_jpeg_segments(
    input: &[u8],
    mut visit: impl FnMut(Option<u8>, &[u8], &[u8]),
) -> Result<Report, ScanError> {
    let mut remaining = input.strip_prefix(b"\xff\xd8").ok_or(ScanError::NotJpeg)?;
    let mut report = Report::new();
    let mut saw_frame = false;
    let mut saw_scan = false;
    let mut in_scan = false;
    let mut saw_jfif = false;

    // ITU-T T.81, Annex B. Outside entropy-coded data, each marker starts
    // with FF and may have FF fill bytes. SOS switches to entropy mode.
    for _ in 0..MAX_MARKERS {
        if in_scan {
            let entropy = remaining;
            remaining = skip_entropy(remaining)?;
            visit(None, &entropy[..entropy.len() - remaining.len()], &[]);
        }
        let segment = remaining;
        let marker = take_marker(&mut remaining)?;
        match marker {
            0xd9 if saw_frame && saw_scan => {
                visit(
                    Some(marker),
                    &segment[..segment.len() - remaining.len()],
                    &[],
                );
                if !remaining.is_empty() {
                    report.record(MetadataKind::TrailingData, Status::Present);
                    report.add_detail(
                        MetadataKind::TrailingData,
                        "Bytes",
                        &remaining.len().to_string(),
                    );
                }
                return Ok(report);
            }
            0xd0..=0xd7 if in_scan => {
                visit(
                    Some(marker),
                    &segment[..segment.len() - remaining.len()],
                    &[],
                );
                continue;
            }
            0x01 => {
                report.record(MetadataKind::Unknown, Status::Present);
                continue;
            }
            0x00..=0xbf | 0xd0..=0xd9 => return Err(ScanError::InvalidJpeg),
            _ => {}
        }

        let payload = take_segment(&mut remaining)?;
        visit(
            Some(marker),
            &segment[..segment.len() - remaining.len()],
            payload,
        );
        in_scan = marker == 0xda || (in_scan && marker == 0xdc);
        match marker {
            0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                let components = usize::from(*payload.get(5).ok_or(ScanError::InvalidJpeg)?);
                if components == 0 || payload.len() != 6 + components * 3 || payload[3..5] == [0, 0]
                {
                    return Err(ScanError::InvalidJpeg);
                }
                saw_frame = true;
            }
            0xda => {
                let components = usize::from(*payload.first().ok_or(ScanError::InvalidJpeg)?);
                if !saw_frame
                    || !(1..=4).contains(&components)
                    || payload.len() != 4 + components * 2
                {
                    return Err(ScanError::InvalidJpeg);
                }
                saw_scan = true;
            }
            0xc4 => validate_tables(payload, huffman_table_length)?,
            0xdb => validate_tables(payload, quantization_table_length)?,
            0xcc if !payload.is_empty() && payload.len() % 2 == 0 => {}
            0xdc | 0xdd if payload.len() == 2 => {}
            0xcc | 0xdc | 0xdd => return Err(ScanError::InvalidJpeg),
            0xe0 if !saw_jfif && is_plain_jfif(payload) => saw_jfif = true,
            0xe1 if payload.starts_with(b"Exif\0\0") => report.inspect_exif(&payload[6..]),
            0xe1 if payload.starts_with(b"http://ns.adobe.com/xap/1.0/\0")
                || payload.starts_with(b"http://ns.adobe.com/xmp/extension/\0") =>
            {
                let data = payload
                    .splitn(2, |&byte| byte == 0)
                    .nth(1)
                    .unwrap_or_default();
                report.add_text(MetadataKind::Xmp, "XMP", data);
            }
            0xe2 if payload.starts_with(b"ICC_PROFILE\0") => {
                report.record(MetadataKind::Icc, Status::Present);
                report.add_detail(
                    MetadataKind::Icc,
                    "ICC segment bytes",
                    &payload.len().saturating_sub(14).to_string(),
                );
            }
            0xed => inspect_app13(payload, &mut report),
            0xfe => report.add_text(MetadataKind::Comment, "Comment", payload),
            _ => {
                report.record(MetadataKind::Unknown, Status::Present);
                report.add_detail(
                    MetadataKind::Unknown,
                    "JPEG marker",
                    &format!("FF{marker:02X}, {} bytes", payload.len()),
                );
            }
        }
    }
    Err(ScanError::MarkerLimit)
}

fn validate_tables(
    mut payload: &[u8],
    table_length: fn(&[u8]) -> Option<usize>,
) -> Result<(), ScanError> {
    if payload.is_empty() {
        return Err(ScanError::InvalidJpeg);
    }
    while !payload.is_empty() {
        let length = table_length(payload).ok_or(ScanError::InvalidJpeg)?;
        payload = payload.get(length..).ok_or(ScanError::InvalidJpeg)?;
    }
    Ok(())
}

fn huffman_table_length(payload: &[u8]) -> Option<usize> {
    let counts = payload.get(1..17)?;
    let count: usize = counts.iter().map(|&count| usize::from(count)).sum();
    (count <= 256).then_some(17 + count)
}

fn quantization_table_length(payload: &[u8]) -> Option<usize> {
    match payload.first()? >> 4 {
        0 => Some(65),
        1 => Some(129),
        _ => None,
    }
}

fn take_marker(remaining: &mut &[u8]) -> Result<u8, ScanError> {
    if remaining.first() != Some(&0xff) {
        return Err(ScanError::InvalidJpeg);
    }
    while let Some((&byte, rest)) = remaining.split_first() {
        *remaining = rest;
        if byte != 0xff {
            return Ok(byte);
        }
    }
    Err(ScanError::InvalidJpeg)
}

fn take_segment<'a>(remaining: &mut &'a [u8]) -> Result<&'a [u8], ScanError> {
    let length = remaining.get(..2).ok_or(ScanError::InvalidJpeg)?;
    let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
    let payload = remaining.get(2..length).ok_or(ScanError::InvalidJpeg)?;
    *remaining = &remaining[length..];
    Ok(payload)
}

fn skip_entropy(mut remaining: &[u8]) -> Result<&[u8], ScanError> {
    loop {
        let marker_start = remaining
            .iter()
            .position(|&byte| byte == 0xff)
            .ok_or(ScanError::InvalidJpeg)?;
        let marker = &remaining[marker_start..];
        let mut after_marker = marker;
        if take_marker(&mut after_marker)? != 0 {
            return Ok(marker);
        }
        remaining = after_marker;
    }
}

pub(super) fn is_plain_jfif(payload: &[u8]) -> bool {
    // Only allow the fixed, thumbnail-free JFIF header emitted by encoders.
    payload.len() == 14
        && payload.starts_with(b"JFIF\0\x01")
        && payload[6] <= 2
        && payload[7] <= 2
        && payload[8..10] != [0, 0]
        && payload[10..12] != [0, 0]
        && payload[12..] == [0, 0]
}

fn inspect_app13(payload: &[u8], report: &mut Report) {
    let Some(mut resources) = payload.strip_prefix(b"Photoshop 3.0\0") else {
        report.record(MetadataKind::Iptc, Status::Unverified);
        report.record(MetadataKind::Unknown, Status::Present);
        return;
    };

    while !resources.is_empty() {
        let Some((id, data, rest)) = photoshop_resource(resources) else {
            for kind in [MetadataKind::Iptc, MetadataKind::Icc, MetadataKind::Xmp] {
                report.record(kind, Status::Unverified);
            }
            report.record(MetadataKind::Unknown, Status::Present);
            return;
        };
        let kind = match id {
            0x0404 => MetadataKind::Iptc,
            0x040f => MetadataKind::Icc,
            0x0424 => MetadataKind::Xmp,
            _ => MetadataKind::Unknown,
        };
        report.record(kind, Status::Present);
        match kind {
            MetadataKind::Xmp => report.add_text(kind, "XMP", data),
            MetadataKind::Iptc => report.inspect_iptc(data),
            _ => report.add_detail(
                kind,
                "Photoshop resource",
                &format!("0x{id:04X}, {} bytes", data.len()),
            ),
        }
        resources = rest;
    }
}

fn photoshop_resource(input: &[u8]) -> Option<(u16, &[u8], &[u8])> {
    // Adobe Photoshop File Formats Specification, Image Resource Blocks.
    let body = input.strip_prefix(b"8BIM")?;
    let id = u16::from_be_bytes([*body.first()?, *body.get(1)?]);
    let name_length = usize::from(*body.get(2)?) + 1;
    let size_offset = 2 + name_length.next_multiple_of(2);
    let size_bytes: [u8; 4] = body.get(size_offset..size_offset + 4)?.try_into().ok()?;
    let size = usize::try_from(u32::from_be_bytes(size_bytes)).ok()?;
    let padded_size = size.checked_add(size % 2)?;
    let end = (size_offset + 4).checked_add(padded_size)?;
    Some((
        id,
        body.get(size_offset + 4..size_offset + 4 + size)?,
        body.get(end..)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(4, 3, image::Rgb([10, 20, 30]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&image)
            .unwrap();
        jpeg
    }

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut segment = vec![0xff, marker];
        segment.extend(u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
        segment.extend(payload);
        segment
    }

    fn with_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut jpeg = jpeg();
        jpeg.splice(2..2, segment(marker, payload));
        jpeg
    }

    #[test]
    fn scans_encoded_jpeg_and_reports_all_absences() {
        assert!(scan(&jpeg()).unwrap().passed());
    }

    #[test]
    fn reads_exif_orientation_and_gps() {
        // Little-endian TIFF with Orientation and a pointer to a GPS IFD
        // containing the GPS version tag. No personal location data.
        let exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x02\0\
            \x12\x01\x03\0\x01\0\0\0\x06\0\0\0\
            \x25\x88\x04\0\x01\0\0\0\x26\0\0\0\
            \0\0\0\0\x01\0\
            \0\0\x01\0\x04\0\0\0\x02\x03\0\0\0\0\0\0";
        let report = scan(&with_segment(0xe1, exif)).unwrap();
        assert_eq!(report.orientation, Some(6));
        assert_eq!(report.status(MetadataKind::Exif), Status::Present);
        assert_eq!(report.status(MetadataKind::Gps), Status::Present);
        assert!(!report.passed());
    }

    #[test]
    fn malformed_exif_is_unverified() {
        let report = scan(&with_segment(0xe1, b"Exif\0\0")).unwrap();
        assert_eq!(report.status(MetadataKind::Exif), Status::Unverified);
        assert_eq!(report.status(MetadataKind::Gps), Status::Unverified);
        assert!(report.orientation_unverified());
        assert!(!report.passed());
    }

    #[test]
    fn exif_budget_charges_ordinary_ascii_storage() {
        // Little-endian IFD with a six-byte ImageDescription stored out of line.
        let exif = b"II\x2a\0\x08\0\0\0\x01\0\x0e\x01\x02\0\x06\0\0\0\x1a\0\0\0\0\0\0\0Photo\0";
        assert!(exif_fits_budget(exif, 256).is_some());
        assert!(exif_fits_budget(exif, 64).is_none());
    }

    #[test]
    fn orientation_distinguishes_absent_invalid_and_duplicate() {
        let absent = b"Exif\0\0II\x2a\0\x08\0\0\0\0\0\0\0\0\0";
        let valid = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\
            \x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
        let report = scan(&with_segment(0xe1, absent)).unwrap();
        assert_eq!(report.orientation, None);
        assert!(!report.orientation_unverified());

        let mut invalid = valid.to_vec();
        invalid[24] = 9;
        let report = scan(&with_segment(0xe1, &invalid)).unwrap();
        assert_eq!(report.orientation, None);
        assert!(report.orientation_unverified());

        let mut duplicate_segment = with_segment(0xe1, valid);
        duplicate_segment.splice(2..2, segment(0xe1, valid));
        let report = scan(&duplicate_segment).unwrap();
        assert_eq!(report.orientation, None);
        assert!(report.orientation_unverified());

        let mut duplicate_tag = valid.to_vec();
        duplicate_tag[14] = 2;
        duplicate_tag.splice(28..28, valid[16..28].iter().copied());
        let report = scan(&with_segment(0xe1, &duplicate_tag)).unwrap();
        assert_eq!(report.orientation, None);
        assert!(report.orientation_unverified());
    }

    #[test]
    fn detects_supported_metadata_and_unclassified_segments() {
        let cases: &[(u8, &[u8], MetadataKind)] = &[
            (
                0xe1,
                b"http://ns.adobe.com/xap/1.0/\0<xmp/>",
                MetadataKind::Xmp,
            ),
            (
                0xe1,
                b"http://ns.adobe.com/xmp/extension/\0test",
                MetadataKind::Xmp,
            ),
            (0xe2, b"ICC_PROFILE\0\x01\x01test", MetadataKind::Icc),
            (
                0xed,
                b"Photoshop 3.0\08BIM\x04\x04\0\0\0\0\0\x04test",
                MetadataKind::Iptc,
            ),
            (0xfe, b"comment", MetadataKind::Comment),
            (0xe5, b"unclassified", MetadataKind::Unknown),
        ];
        for &(marker, payload, kind) in cases {
            let report = scan(&with_segment(marker, payload)).unwrap();
            assert_eq!(report.status(kind), Status::Present);
            assert!(!report.passed());
        }
    }

    #[test]
    fn scans_metadata_after_entropy_and_reports_trailing_data() {
        let mut jpeg = jpeg();
        let end = jpeg.len() - 2;
        jpeg.splice(end..end, segment(0xfe, b"comment"));
        jpeg.extend(b"trailer");
        let report = scan(&jpeg).unwrap();
        assert_eq!(report.status(MetadataKind::Comment), Status::Present);
        assert_eq!(report.status(MetadataKind::TrailingData), Status::Present);
    }

    #[test]
    fn malformed_photoshop_resources_mark_unreadable_categories() {
        let input = with_segment(0xed, b"Photoshop 3.0\08BIM\x04\x04\0\0\0\0\0\x04test8BIM");
        let report = scan(&input).unwrap();
        assert_eq!(report.status(MetadataKind::Iptc), Status::Present);
        assert_eq!(report.status(MetadataKind::Xmp), Status::Unverified);
        assert_eq!(report.status(MetadataKind::Icc), Status::Unverified);
        assert_eq!(report.status(MetadataKind::Unknown), Status::Present);
        assert!(!report.passed());
    }

    #[test]
    fn marker_walk_handles_scan_stuffing_restarts_and_later_scans() {
        // A marker framing fixture for the scanner. Pixel decoding is tested
        // separately with encoder-produced JPEGs.
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend(segment(0xc2, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]));
        bytes.extend(segment(0xda, &[1, 1, 0, 0, 0, 0]));
        bytes.extend([0x12, 0xff, 0x00, 0xfe, 0xff, 0xd0, 0x12]);
        bytes.extend(segment(0xfe, b"between scans"));
        bytes.extend(segment(0xda, &[1, 1, 0, 1, 63, 0]));
        bytes.extend([0x12, 0xff, 0xd9]);
        let report = scan(&bytes).unwrap();
        assert_eq!(report.status(MetadataKind::Comment), Status::Present);
        assert_eq!(
            report.status(MetadataKind::TrailingData),
            Status::NotDetected
        );
    }

    #[test]
    fn rejects_truncated_segments_and_missing_end_marker() {
        assert!(matches!(scan(b"PNG"), Err(ScanError::NotJpeg)));
        assert!(matches!(
            scan(b"\xff\xd8\xff\xe1\x00\x07EX"),
            Err(ScanError::InvalidJpeg)
        ));
        let jpeg = jpeg();
        assert!(matches!(
            scan(&jpeg[..jpeg.len() - 2]),
            Err(ScanError::InvalidJpeg)
        ));
    }
}
