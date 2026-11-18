//! Bounded Office Open XML package inspection and rewriting.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read, Write},
    ops::Range,
};

use quick_xml::{
    XmlVersion,
    events::{BytesStart, Event},
    name::ResolveResult,
    reader::NsReader,
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use super::{Error, MetadataKind, Report, Status, report, unsupported};

const RELS: &str = "_rels/.rels";
const TYPES: &str = "[Content_Types].xml";
const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const TYPES_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const CORE_NS: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const EXT_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";
const CUSTOM_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties";
const WORD_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const OFFICE_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const CORE_REL: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const MAX_ENTRIES: usize = 4096;
const MAX_EXPANDED: usize = 128 * 1024 * 1024;
const MAX_CONTROL_XML: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Properties {
    Core,
    Extended,
    Custom,
}

impl Properties {
    fn from_relationship(value: &str) -> Option<Self> {
        if value == CORE_REL {
            return Some(Self::Core);
        }
        match value.strip_prefix(OFFICE_REL) {
            Some("extended-properties") => Some(Self::Extended),
            Some("custom-properties") => Some(Self::Custom),
            _ => None,
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Core => "application/vnd.openxmlformats-package.core-properties+xml",
            Self::Extended => {
                "application/vnd.openxmlformats-officedocument.extended-properties+xml"
            }
            Self::Custom => "application/vnd.openxmlformats-officedocument.custom-properties+xml",
        }
    }

    fn kind(self) -> MetadataKind {
        match self {
            Self::Core => MetadataKind::OfficeCore,
            Self::Extended => MetadataKind::OfficeExtended,
            Self::Custom => MetadataKind::OfficeCustom,
        }
    }

    fn root(self) -> (&'static str, &'static str) {
        match self {
            Self::Core => (CORE_NS, "coreProperties"),
            Self::Extended => (EXT_NS, "Properties"),
            Self::Custom => (CUSTOM_NS, "Properties"),
        }
    }
}

const GROUPS: [Properties; 3] = [Properties::Core, Properties::Extended, Properties::Custom];

fn invalid() -> Error {
    unsupported("Invalid or unsupported Office Open XML package")
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':', '%', '#', '?'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|segment| {
            !segment.is_empty() && !segment.ends_with('.') && !segment.ends_with(' ')
                || matches!(path, RELS) && matches!(segment, ".rels")
        })
}

fn part_name(path: &str) -> Result<String, Error> {
    let path = path.strip_prefix('/').unwrap_or(path);
    if !valid_path(path) {
        return Err(invalid());
    }
    Ok(path.to_ascii_lowercase())
}

struct Part {
    name: String,
    bytes: Vec<u8>,
}

fn zip_entry_count(bytes: &[u8]) -> Result<usize, Error> {
    // The ZIP reader collapses exactly duplicate names. Compare its count with
    // the single-disk ZIP32 end record before trusting the package index.
    let tail = &bytes[bytes.len().saturating_sub(22 + u16::MAX as usize)..];
    let end = tail
        .windows(22)
        .enumerate()
        .rfind(|(position, record)| {
            record.starts_with(b"PK\x05\x06")
                && position + 22 + u16::from_le_bytes([record[20], record[21]]) as usize
                    == tail.len()
        })
        .map(|(_, record)| record)
        .ok_or_else(invalid)?;
    let count = u16::from_le_bytes([end[10], end[11]]) as usize;
    if end[4..8] != [0; 4] || end[8..10] != end[10..12] || count > MAX_ENTRIES {
        return Err(unsupported(
            "Office ZIP structure or entry count is unsupported",
        ));
    }
    Ok(count)
}

fn read_parts(bytes: &[u8]) -> Result<BTreeMap<String, Part>, Error> {
    if bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
        return Err(unsupported(
            "Encrypted or legacy Office packages are unsupported",
        ));
    }
    let expected_entries = zip_entry_count(bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid())?;
    if archive.offset() != 0 || archive.len() != expected_entries {
        return Err(unsupported(
            "Office package has nonstandard or ambiguous ZIP entries",
        ));
    }

    let mut parts = BTreeMap::new();
    let mut remaining = MAX_EXPANDED;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|_| invalid())?;
        let name = std::str::from_utf8(file.name_raw()).map_err(|_| invalid())?;
        let directory = file.is_dir();
        let path = if directory {
            name.strip_suffix('/').ok_or_else(invalid)?
        } else {
            name
        };
        if !valid_path(path) || file.is_symlink() {
            return Err(invalid());
        }
        let name = name.to_owned();
        let path = path.to_ascii_lowercase();
        if directory {
            if file.size() != 0 {
                return Err(invalid());
            }
            continue;
        }
        if file.size() > remaining as u64 {
            return Err(unsupported(
                "Office package exceeds the expanded size limit",
            ));
        }

        let mut contents = Vec::new();
        (&mut file)
            .take(remaining as u64 + 1)
            .read_to_end(&mut contents)
            .map_err(|_| invalid())?;
        if contents.len() > remaining || contents.len() as u64 != file.size() {
            return Err(invalid());
        }
        remaining -= contents.len();
        if parts
            .insert(
                path,
                Part {
                    name,
                    bytes: contents,
                },
            )
            .is_some()
        {
            return Err(unsupported("Office package has ambiguous part names"));
        }
    }
    Ok(parts)
}

enum Xml<'a> {
    Open {
        namespace: &'a str,
        name: &'a str,
        tag: BytesStart<'a>,
        depth: usize,
        start: usize,
    },
    Text(&'a str),
    Close {
        depth: usize,
        end: usize,
    },
}

fn walk_xml(
    bytes: &[u8],
    mut visit: impl FnMut(Xml<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    // NsReader resolves element prefixes independently of the qualified spelling.
    let mut reader = NsReader::from_str(std::str::from_utf8(bytes).map_err(|_| invalid())?);
    reader.config_mut().check_comments = true;
    let mut depth = 0;
    let mut root = false;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|_| invalid())?;
        let empty = matches!(event, Event::Empty(_));
        let end = reader.buffer_position() as usize;
        match event {
            Event::Start(tag) | Event::Empty(tag) => {
                if depth == 0 {
                    if root {
                        return Err(invalid());
                    }
                    root = true;
                }
                if depth >= 128 {
                    return Err(unsupported("Office XML exceeds the nesting limit"));
                }
                let (namespace, local) = reader.resolver().resolve_element(tag.name());
                let namespace = match namespace {
                    ResolveResult::Bound(ns) => ns.into_inner(),
                    ResolveResult::Unbound => "",
                    ResolveResult::Unknown(_) => return Err(invalid()),
                };
                let name = local.as_ref();
                visit(Xml::Open {
                    namespace,
                    name,
                    tag: tag.borrow(),
                    depth,
                    start,
                })?;
                if empty {
                    visit(Xml::Close { depth, end })?;
                } else {
                    depth += 1;
                }
            }
            Event::End(_) => {
                depth = depth.checked_sub(1).ok_or_else(invalid)?;
                visit(Xml::Close { depth, end })?;
            }
            Event::Text(value) => {
                let value = value.xml10_content();
                if depth == 0 && !value.trim().is_empty() {
                    return Err(invalid());
                }
                visit(Xml::Text(&value))?;
            }
            Event::CData(value) if depth > 0 => visit(Xml::Text(&value.xml10_content()))?,
            Event::GeneralRef(value) if depth > 0 => {
                let mut buffer = [0; 4];
                let reference = value.resolve_char_ref().map_err(|_| invalid())?;
                let value = if let Some(character) = reference {
                    character.encode_utf8(&mut buffer)
                } else {
                    match value.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "apos" => "'",
                        "quot" => "\"",
                        _ => return Err(invalid()),
                    }
                };
                visit(Xml::Text(value))?;
            }
            Event::Decl(decl) => {
                if let Some(encoding) = decl.encoding()
                    && !encoding
                        .map_err(|_| invalid())?
                        .eq_ignore_ascii_case("utf-8")
                {
                    return Err(unsupported("Office XML encoding is unsupported"));
                }
            }
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof if root && depth == 0 => return Ok(()),
            _ => return Err(invalid()),
        }
    }
}

fn attributes(tag: &BytesStart<'_>) -> Result<BTreeMap<String, String>, Error> {
    tag.attributes()
        .map(|attr| {
            let attr = attr.map_err(|_| invalid())?;
            let key = attr.key.as_ref();
            let value = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|_| invalid())?;
            Ok((key.to_owned(), value.into_owned()))
        })
        .collect()
}

struct Element {
    name: String,
    attributes: BTreeMap<String, String>,
    range: Range<usize>,
}

impl Element {
    fn get(&self, key: &str) -> Result<&str, Error> {
        self.attributes
            .get(key)
            .map(String::as_str)
            .ok_or_else(invalid)
    }
}

fn control(
    bytes: &[u8],
    expected_namespace: &str,
    expected_root: &str,
) -> Result<Vec<Element>, Error> {
    if bytes.len() > MAX_CONTROL_XML {
        return Err(unsupported("Office package control XML is too large"));
    }
    let mut elements = Vec::<Element>::new();
    walk_xml(bytes, |event| {
        match event {
            Xml::Open {
                namespace,
                name,
                tag,
                depth,
                start,
            } if namespace == expected_namespace => match depth {
                0 if name == expected_root => {}
                1 => elements.push(Element {
                    name: name.to_owned(),
                    attributes: attributes(&tag)?,
                    range: start..start,
                }),
                _ => return Err(invalid()),
            },
            Xml::Open { .. } => return Err(invalid()),
            Xml::Close { depth: 1, end } => {
                elements.last_mut().ok_or_else(invalid)?.range.end = end;
            }
            Xml::Text(value) if !value.trim().is_empty() => return Err(invalid()),
            _ => {}
        }
        Ok(())
    })?;
    Ok(elements)
}

fn omit_ranges(bytes: &[u8], mut ranges: Vec<Range<usize>>) -> Vec<u8> {
    ranges.sort_unstable_by_key(|range| range.start);
    let mut output = Vec::new();
    let mut offset = 0;
    for range in ranges {
        output.extend_from_slice(&bytes[offset..range.start]);
        offset = range.end;
    }
    output.extend_from_slice(&bytes[offset..]);
    output
}

struct PropertyPart {
    path: String,
    relationship: Range<usize>,
}

struct Package {
    parts: BTreeMap<String, Part>,
    properties: BTreeMap<Properties, PropertyPart>,
    overrides: BTreeMap<String, Element>,
    signed: bool,
    xml_parts: BTreeSet<String>,
}

fn package(bytes: &[u8], format: &str) -> Result<Package, Error> {
    let parts = read_parts(bytes)?;
    let bytes_for = |name: &str| {
        parts
            .get(name)
            .map(|part| part.bytes.as_slice())
            .ok_or_else(invalid)
    };
    let mut overrides = BTreeMap::new();
    let mut defaults = BTreeMap::new();
    for element in control(bytes_for(&TYPES.to_ascii_lowercase())?, TYPES_NS, "Types")? {
        let content_type = element.get("ContentType")?.to_owned();
        match element.name.as_str() {
            "Override" => {
                let name = element.get("PartName")?;
                if !name.starts_with('/') {
                    return Err(invalid());
                }
                let path = part_name(name)?;
                if !parts.contains_key(&path) || overrides.insert(path, element).is_some() {
                    return Err(invalid());
                }
            }
            "Default" => {
                if defaults
                    .insert(element.get("Extension")?.to_ascii_lowercase(), content_type)
                    .is_some()
                {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
    }
    let content_type = |path: &str| -> Result<&str, Error> {
        if let Some(element) = overrides.get(path) {
            return element.get("ContentType");
        }
        let extension = path.rsplit_once('.').ok_or_else(invalid)?.1;
        defaults
            .get(extension)
            .map(String::as_str)
            .ok_or_else(invalid)
    };

    let mut ids = BTreeSet::new();
    let mut properties = BTreeMap::new();
    let mut main = false;
    let mut signed = parts.keys().any(|name| name.starts_with("_xmlsignatures/"));
    for element in control(bytes_for(RELS)?, REL_NS, "Relationships")? {
        if element.name != "Relationship" || !ids.insert(element.get("Id")?.to_owned()) {
            return Err(invalid());
        }
        let relation = element.get("Type")?;
        signed |= relation.starts_with(
            "http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/",
        );
        let group = Properties::from_relationship(relation);
        let is_main = relation.strip_prefix(OFFICE_REL) == Some("officeDocument");
        let mode = element
            .attributes
            .get("TargetMode")
            .map(String::as_str)
            .unwrap_or("Internal");
        if mode == "External" && group.is_none() && !is_main {
            continue;
        }
        if mode != "Internal" {
            return Err(invalid());
        }
        let path = part_name(element.get("Target")?)?;
        if !parts.contains_key(&path) {
            return Err(invalid());
        }

        if is_main {
            let (expected, namespace, root) = match format {
                "DOCX" => (
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
                    WORD_NS,
                    "document",
                ),
                "XLSX" => (
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
                    SHEET_NS,
                    "workbook",
                ),
                _ => return Err(invalid()),
            };
            if main || content_type(&path)? != expected {
                return Err(invalid());
            }
            walk_xml(bytes_for(&path)?, |event| {
                if let Xml::Open {
                    namespace: actual_ns,
                    name,
                    depth: 0,
                    ..
                } = event
                    && (actual_ns, name) != (namespace, root)
                {
                    return Err(unsupported("Office main part has an unsupported XML root"));
                }
                Ok(())
            })?;
            main = true;
        }
        if let Some(group) = group
            && (content_type(&path)? != group.content_type()
                || properties
                    .insert(
                        group,
                        PropertyPart {
                            path,
                            relationship: element.range,
                        },
                    )
                    .is_some())
        {
            return Err(invalid());
        }
    }
    if !main {
        return Err(unsupported(
            "Office document type is unsupported or does not match its extension",
        ));
    }
    let mut xml_parts = BTreeSet::new();
    // Refuse orphaned properties even if a conventional path is mislabeled.
    for path in parts
        .keys()
        .filter(|path| !path.ends_with(".rels") && !path.eq_ignore_ascii_case(TYPES))
    {
        let kind = content_type(path).ok();
        if path.ends_with(".xml")
            || kind.is_some_and(|value| value.ends_with("+xml") || value == "application/xml")
        {
            xml_parts.insert(path.clone());
        }
        signed |= kind.is_some_and(|value| {
            value.starts_with("application/vnd.openxmlformats-package.digital-signature-")
        });
        for (group, conventional) in GROUPS.into_iter().zip([
            "docprops/core.xml",
            "docprops/app.xml",
            "docprops/custom.xml",
        ]) {
            if (kind == Some(group.content_type()) || path == conventional)
                && properties
                    .get(&group)
                    .is_none_or(|property| &property.path != path)
            {
                return Err(unsupported(
                    "Office package has unlinked document properties",
                ));
            }
        }
    }
    Ok(Package {
        parts,
        properties,
        overrides,
        signed,
        xml_parts,
    })
}

fn field_name(group: Properties, namespace: &str, name: &str) -> bool {
    match group {
        Properties::Core => true,
        Properties::Custom => namespace == CUSTOM_NS && name == "property",
        Properties::Extended => namespace == EXT_NS && matches!(name, "Company" | "Manager"),
    }
}

fn fields(
    bytes: &[u8],
    group: Properties,
    report: &mut Report,
) -> Result<Vec<Range<usize>>, Error> {
    if bytes.len() > MAX_CONTROL_XML {
        return Err(unsupported("Office property XML is too large"));
    }
    let mut ranges = Vec::new();
    let mut field: Option<(String, String, usize)> = None;
    let (expected_namespace, expected_root) = group.root();
    walk_xml(bytes, |event| {
        match event {
            Xml::Open {
                namespace,
                name,
                depth: 0,
                ..
            } if (namespace, name) != (expected_namespace, expected_root) => return Err(invalid()),
            Xml::Open {
                namespace,
                name,
                tag,
                depth: 1,
                start,
            } if field_name(group, namespace, name) => {
                let name = if group == Properties::Custom {
                    attributes(&tag)?.remove("name").ok_or_else(invalid)?
                } else {
                    name.to_owned()
                };
                field = Some((name, String::new(), start));
            }
            Xml::Text(value) => {
                if let Some((_, text, _)) = &mut field {
                    text.push_str(value);
                }
            }
            Xml::Close { depth: 1, end } => {
                if let Some((name, value, start)) = field.take() {
                    ranges.push(start..end);
                    report.add_text(group.kind(), &name, value.trim().as_bytes());
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    let present = if group == Properties::Extended {
        !ranges.is_empty()
    } else {
        true
    };
    if present {
        report.record(group.kind(), Status::Present);
    }
    Ok(ranges)
}

pub(super) fn inspect(bytes: &[u8], format: &str) -> Result<Report, Error> {
    let package = package(bytes, format)?;
    let mut result = report(format, &GROUPS.map(Properties::kind));
    for (group, part) in package.properties {
        fields(&package.parts[&part.path].bytes, group, &mut result)?;
    }
    Ok(result)
}

fn word_property_field(bytes: &[u8]) -> Result<bool, Error> {
    let mut instruction = String::new();
    let mut collecting = false;
    let mut dependency = false;
    walk_xml(bytes, |event| {
        match event {
            Xml::Open {
                namespace: WORD_NS,
                name: "fldSimple",
                tag,
                ..
            } => {
                // Attribute prefixes are arbitrary. Match the local name conservatively.
                let attributes = attributes(&tag)?;
                let instructions: Vec<_> = attributes
                    .iter()
                    .filter(|(name, _)| name.rsplit(':').next() == Some("instr"))
                    .collect();
                dependency |= instructions.len() != 1
                    || instructions[0]
                        .1
                        .trim_start()
                        .split_ascii_whitespace()
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case("DOCPROPERTY"));
            }
            Xml::Open {
                namespace: WORD_NS,
                name: "instrText",
                ..
            } => collecting = true,
            Xml::Text(text) if collecting => {
                // A complex field may split its instruction across XML runs.
                instruction.push_str(text);
                if instruction.len() > MAX_CONTROL_XML {
                    return Err(unsupported("Office field instruction is too large"));
                }
            }
            Xml::Close { .. } => collecting = false,
            _ => {}
        }
        Ok(())
    })?;
    Ok(dependency || instruction.to_ascii_uppercase().contains("DOCPROPERTY"))
}

// Resolve POSIX-style OPC paths without opening anything outside the package.
fn relationship_target(target: &str, directory: &str) -> Result<String, Error> {
    let target = target.split('#').next().unwrap_or_default();
    let (directory, target) = match target.strip_prefix('/') {
        Some(absolute) => ("", absolute),
        None => (directory, target),
    };
    let mut segments: Vec<_> = directory
        .split('/')
        .filter(|name| !name.is_empty())
        .collect();
    for part in target.split('/') {
        match part {
            "." => {}
            ".." => {
                segments.pop().ok_or_else(invalid)?;
            }
            _ => segments.push(part),
        }
    }
    part_name(&segments.join("/"))
}

fn check_removed_relationships(
    parts: &BTreeMap<String, Part>,
    removing: &BTreeSet<String>,
    removed_relationships: &[Range<usize>],
) -> Result<(), Error> {
    if removing.is_empty() {
        return Ok(());
    }
    for (path, part) in parts.iter().filter(|(path, _)| path.ends_with(".rels")) {
        let directory = if path == RELS {
            ""
        } else {
            let (directory, part) = path.rsplit_once('/').ok_or_else(invalid)?;
            let directory = if directory == "_rels" {
                ""
            } else {
                directory.strip_suffix("/_rels").ok_or_else(invalid)?
            };
            let owner = part.strip_suffix(".rels").ok_or_else(invalid)?;
            let owner = if directory.is_empty() {
                owner.to_owned()
            } else {
                format!("{directory}/{owner}")
            };
            if removing.contains(&owner) {
                return Err(unsupported(
                    "Office property part has dependent relationships",
                ));
            }
            directory
        };
        for element in control(&part.bytes, REL_NS, "Relationships")? {
            if element.name != "Relationship" {
                return Err(invalid());
            }
            if path == RELS && removed_relationships.contains(&element.range) {
                continue;
            }
            match element.attributes.get("TargetMode").map(String::as_str) {
                Some("External") => continue,
                None | Some("Internal") => {}
                _ => return Err(invalid()),
            }
            let target = element.get("Target")?;
            if target.starts_with('#') {
                continue; // A fragment-only reference points back to its own part.
            }
            let target = relationship_target(target, directory)?;
            if removing.contains(&target) {
                return Err(unsupported(
                    "Office document content links to a property part",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn clean(bytes: &[u8], format: &str) -> Result<Vec<u8>, Error> {
    let mut package = package(bytes, format)?;
    if package.signed {
        return Err(unsupported("Signed Office packages cannot be cleaned"));
    }
    let mut result = report(format, &GROUPS.map(Properties::kind));
    let mut remove = BTreeSet::new();
    let mut relationships = Vec::new();
    let mut overrides = Vec::new();
    for (group, part) in &package.properties {
        let bytes = &mut package.parts.get_mut(&part.path).ok_or_else(invalid)?.bytes;
        let ranges = fields(bytes, *group, &mut result)?;
        if *group == Properties::Extended {
            if !ranges.is_empty() {
                *bytes = omit_ranges(bytes, ranges);
            }
        } else {
            remove.insert(part.path.clone());
            relationships.push(part.relationship.clone());
            if let Some(element) = package.overrides.get(&part.path) {
                overrides.push(element.range.clone());
            }
        }
    }
    if format == "DOCX" && !result.passed() {
        for path in &package.xml_parts {
            if word_property_field(&package.parts[path].bytes)? {
                return Err(unsupported("Word fields depend on document properties"));
            }
        }
    }
    check_removed_relationships(&package.parts, &remove, &relationships)?;
    if !relationships.is_empty() {
        let part = package.parts.get_mut(RELS).ok_or_else(invalid)?;
        part.bytes = omit_ranges(&part.bytes, relationships);
    }
    if !overrides.is_empty() {
        let part = package
            .parts
            .get_mut(&TYPES.to_ascii_lowercase())
            .ok_or_else(invalid)?;
        part.bytes = omit_ranges(&part.bytes, overrides);
    }
    let mut output = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (path, part) in package.parts {
        if !remove.contains(&path) {
            output
                .start_file(part.name, options)
                .map_err(|_| invalid())?;
            output.write_all(&part.bytes).map_err(|_| invalid())?;
        }
    }
    Ok(output.finish().map_err(|_| invalid())?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: &str = r#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:creator>A &amp; B</dc:creator></cp:coreProperties>"#;
    const APP: &str = r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Office</Application><Company>Internal</Company><Manager/></Properties>"#;
    const CUSTOM: &str = r#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property name="Project"><vt:lpwstr>Secret</vt:lpwstr></property></Properties>"#;
    const WORD: &str = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Preserve</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#;
    const BOOK: &str = r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets/></workbook>"#;
    const SHEET: &str = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" s="1"><f>1+1</f><v>2</v></c></row></sheetData></worksheet>"#;

    fn fixture(format: &str, content: &str, extra: Option<(&str, &str)>) -> Vec<u8> {
        let (name, mime) = if format == "DOCX" {
            ("word/document.xml", "wordprocessingml.document.main")
        } else {
            ("xl/workbook.xml", "spreadsheetml.sheet.main")
        };
        let rels = format!(
            r#"<Relationships xmlns="{REL_NS}"><Relationship Id="r1" Type="{OFFICE_REL}officeDocument" Target="{name}"/><Relationship Id="r2" Type="{CORE_REL}" Target="/custom-location/core.xml"/><Relationship Id="r3" Type="{OFFICE_REL}extended-properties" Target="docProps/app.xml"/><Relationship Id="r4" Type="{OFFICE_REL}custom-properties" Target="docProps/custom.xml"/></Relationships>"#
        );
        let types = format!(
            r#"<Types xmlns="{TYPES_NS}"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/{name}" ContentType="application/vnd.openxmlformats-officedocument.{mime}+xml"/><Override PartName="/custom-location/core.xml" ContentType="{}"/><Override PartName="/docProps/app.xml" ContentType="{}"/><Override PartName="/docProps/custom.xml" ContentType="{}"/></Types>"#,
            Properties::Core.content_type(),
            Properties::Extended.content_type(),
            Properties::Custom.content_type()
        );
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let files = [
            (RELS, rels.as_str()),
            (TYPES, types.as_str()),
            (name, content),
            ("custom-location/core.xml", CORE),
            ("docProps/app.xml", APP),
            ("docProps/custom.xml", CUSTOM),
        ];
        for (name, value) in files.into_iter().chain(extra) {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(value.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn word_and_excel_preserve_content_parts() {
        for (format, name, content, extra) in [
            ("DOCX", "word/document.xml", WORD, None),
            (
                "XLSX",
                "xl/workbook.xml",
                BOOK,
                Some(("xl/worksheets/sheet1.xml", SHEET)),
            ),
        ] {
            let original = fixture(format, content, extra);
            let report = inspect(&original, format).unwrap();
            assert!(!report.passed());
            assert!(
                report
                    .details
                    .iter()
                    .any(|detail| detail.name == "creator" && detail.value == "A & B")
            );
            assert!(
                report
                    .details
                    .iter()
                    .any(|detail| detail.name == "Project" && detail.value == "Secret")
            );
            let output = clean(&original, format).unwrap();
            assert!(inspect(&output, format).unwrap().passed());
            let parts = read_parts(&output).unwrap();
            assert_eq!(parts[name].bytes, content.as_bytes());
            assert_eq!(
                std::str::from_utf8(&parts["docprops/app.xml"].bytes).unwrap(),
                APP.replace("<Company>Internal</Company><Manager/>", "")
            );
            assert!(!parts.contains_key("custom-location/core.xml"));
            assert!(!parts.contains_key("docprops/custom.xml"));
            if let Some((name, value)) = extra {
                assert_eq!(parts[name].bytes, value.as_bytes());
            }
        }
    }

    #[test]
    fn refuses_word_property_field_dependencies() {
        for instruction in [
            r#"<w:fldSimple w:instr=" DOCPROPERTY Author "/>"#,
            r#"<w:r><w:instrText> DOC</w:instrText></w:r><w:r><w:instrText>PROPERTY Project </w:instrText></w:r>"#,
        ] {
            let header = format!(r#"<w:hdr xmlns:w="{WORD_NS}">{instruction}</w:hdr>"#);
            let original = fixture("DOCX", WORD, Some(("word/header1.xml", &header)));
            assert!(inspect(&original, "DOCX").is_ok());
            assert!(
                clean(&original, "DOCX")
                    .unwrap_err()
                    .to_string()
                    .contains("depend")
            );
        }
    }

    #[test]
    fn refuses_changes_that_would_leave_dangling_relationships() {
        let rels = format!(
            r#"<Relationships xmlns="{REL_NS}"><Relationship Id="r5" Type="{CORE_REL}" Target="../custom-location/core.xml"/></Relationships>"#
        );
        let original = fixture("DOCX", WORD, Some(("word/_rels/document.xml.rels", &rels)));
        assert!(
            clean(&original, "DOCX")
                .unwrap_err()
                .to_string()
                .contains("links to a property part")
        );
    }

    #[test]
    fn refuses_mismatched_signed_and_unlinked_property_packages() {
        let input = fixture(
            "DOCX",
            WORD,
            Some(("_xmlsignatures/sig1.xml", "<Signature/>")),
        );
        assert!(inspect(&input, "XLSX").is_err());
        let wrong_main = fixture("DOCX", BOOK, None);
        assert!(
            inspect(&wrong_main, "DOCX")
                .unwrap_err()
                .to_string()
                .contains("XML root")
        );
        assert!(
            clean(&input, "DOCX")
                .unwrap_err()
                .to_string()
                .contains("Signed")
        );
        let input = fixture("DOCX", WORD, Some(("docProps/core.xml", CORE)));
        assert!(
            inspect(&input, "DOCX")
                .unwrap_err()
                .to_string()
                .contains("unlinked")
        );
    }
}
