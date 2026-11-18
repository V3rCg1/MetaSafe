use std::collections::HashSet;

use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId};
use quick_xml::{Reader, events::Event};

use super::super::{
    Error,
    metadata::{MetadataKind, Report, Status},
};
use super::{report, unsupported};

const STREAM_LIMIT: usize = 8 * 1024 * 1024;
const XMP_LIMIT: usize = 1024 * 1024;
const MAX_DEPTH: usize = 64;

fn load(bytes: &[u8]) -> Result<Document, Error> {
    // https://docs.rs/lopdf/0.47.0/lopdf/struct.LoadOptions.html
    let document = Document::load_mem_with_options(
        bytes,
        LoadOptions {
            strict: true,
            max_decompressed_size: Some(STREAM_LIMIT),
            ..Default::default()
        },
    )
    .map_err(|_| unsupported("PDF structure is invalid or unsupported"))?;
    if document.was_encrypted() || document.is_encrypted() {
        return Err(unsupported("Encrypted PDF"));
    }
    document
        .catalog()
        .map_err(|_| unsupported("PDF catalog is missing"))?;
    Ok(document)
}

fn info_value(document: &Document, object: &Object) -> Result<Option<String>, Error> {
    let (_, it) = document
        .dereference(object)
        .map_err(|_| unsupported("PDF property reference is invalid"))?;
    let text = match it {
        Object::String(_, _) => lopdf::decode_text_string(it).ok(),
        Object::Name(value) => Some(String::from_utf8_lossy(value).into_owned()),
        Object::Integer(value) => Some(value.to_string()),
        Object::Boolean(value) => Some(value.to_string()),
        Object::Real(value) => Some(value.to_string()),
        _ => None,
    };
    Ok(text)
}

fn xmp_text(document: &Document, catalog: &Dictionary) -> Result<Option<Vec<u8>>, Error> {
    let Ok(metadata) = catalog.get(b"Metadata") else {
        return Ok(None);
    };
    let (_, object) = document
        .dereference(metadata)
        .map_err(|_| unsupported("PDF XMP reference is invalid"))?;
    let stream = object
        .as_stream()
        .map_err(|_| unsupported("PDF XMP is not a stream"))?;
    if stream.content.len() > XMP_LIMIT {
        return Err(unsupported("PDF XMP exceeds the size limit"));
    }
    if !stream.dict.has(b"Filter") {
        return Ok(Some(stream.content.clone()));
    }
    let bytes = stream
        .decompressed_content_with_limit(XMP_LIMIT)
        .map_err(|_| unsupported("PDF XMP encoding is unsupported"))?;
    Ok(Some(bytes))
}

fn add_property(report: &mut Report, kind: MetadataKind, name: &str, value: &[u8]) {
    let end = name
        .char_indices()
        .nth(128)
        .map_or(name.len(), |(index, _)| index);
    report.add_text(kind, &name[..end], value);
}

fn read_xmp(bytes: &[u8], report: &mut Report) -> Result<bool, Error> {
    let invalid_xml = || unsupported("PDF XMP is not supported UTF-8 XML");
    let xml = std::str::from_utf8(bytes).map_err(|_| invalid_xml())?;
    let mut reader = Reader::from_str(xml);
    let mut elements_stack: Vec<(String, String)> = Vec::new();
    let mut pdfa = false;
    let mut elements = 0;
    loop {
        let event = reader.read_event().map_err(|_| invalid_xml())?;
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                if elements_stack.len() >= MAX_DEPTH {
                    return Err(invalid_xml());
                }
                if elements_stack.is_empty() {
                    elements += 1;
                }
                for attribute in element.attributes() {
                    let attribute = attribute.map_err(|_| invalid_xml())?;
                    let key = attribute.key.as_ref();
                    let value = attribute
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|_| invalid_xml())?;
                    if key == "xmlns" || key.starts_with("xmlns:") {
                        pdfa |= value.contains("aiim.org/pdfa/ns/");
                    } else if !key.starts_with("rdf:") {
                        add_property(report, MetadataKind::DocumentXmp, key, value.as_bytes());
                    }
                }
                if matches!(event, Event::Start(_)) {
                    let name = element.name();
                    elements_stack.push((name.as_ref().to_owned(), String::new()));
                }
            }
            Event::End(_) => {
                let (name, value) = elements_stack.pop().ok_or_else(invalid_xml)?;
                let value = value.trim();
                if !value.is_empty() {
                    let property = if name.starts_with("rdf:") {
                        elements_stack
                            .iter()
                            .rev()
                            .find(|(name, _)| !name.starts_with("rdf:"))
                            .map_or(name.as_str(), |(name, _)| name.as_str())
                    } else {
                        &name
                    };
                    add_property(
                        report,
                        MetadataKind::DocumentXmp,
                        property,
                        value.as_bytes(),
                    );
                }
            }
            Event::Text(value) => {
                if let Some((_, text)) = elements_stack.last_mut() {
                    text.push_str(value.as_ref());
                } else if !value.as_ref().trim().is_empty() {
                    return Err(invalid_xml());
                }
            }
            Event::CData(value) => {
                let (_, text) = elements_stack.last_mut().ok_or_else(invalid_xml)?;
                text.push_str(value.as_ref());
            }
            Event::GeneralRef(reference) => {
                let (_, text) = elements_stack.last_mut().ok_or_else(invalid_xml)?;
                if let Some(character) = reference.resolve_char_ref().map_err(|_| invalid_xml())? {
                    text.push(character);
                } else {
                    text.push_str(
                        quick_xml::escape::resolve_predefined_entity(reference.as_ref())
                            .ok_or_else(invalid_xml)?,
                    );
                }
            }
            Event::DocType(_) => return Err(invalid_xml()),
            Event::Eof => break,
            _ => {}
        }
    }
    if elements != 1 || !elements_stack.is_empty() {
        return Err(invalid_xml());
    }
    Ok(pdfa)
}

fn inspect_loaded(document: &Document) -> Result<(Report, bool), Error> {
    let mut report = report(
        "PDF",
        &[MetadataKind::DocumentInfo, MetadataKind::DocumentXmp],
    );
    if let Ok(info) = document.trailer.get(b"Info") {
        let (_, object) = document
            .dereference(info)
            .map_err(|_| unsupported("PDF properties reference is invalid"))?;
        let dictionary = object
            .as_dict()
            .map_err(|_| unsupported("PDF properties are not a dictionary"))?;
        report.record(MetadataKind::DocumentInfo, Status::Present);
        for (name, object) in dictionary.iter() {
            if let Some(value) = info_value(document, object)? {
                add_property(
                    &mut report,
                    MetadataKind::DocumentInfo,
                    &String::from_utf8_lossy(name),
                    value.as_bytes(),
                );
            }
        }
    }

    let catalog = document
        .catalog()
        .map_err(|_| unsupported("PDF catalog is missing"))?;
    let mut pdfa = false;
    if let Some(bytes) = xmp_text(document, catalog)? {
        report.record(MetadataKind::DocumentXmp, Status::Present);
        pdfa = read_xmp(&bytes, &mut report)?;
    }
    Ok((report, pdfa))
}

pub(super) fn inspect(bytes: &[u8]) -> Result<Report, Error> {
    inspect_loaded(&load(bytes)?).map(|(report, _)| report)
}

fn direct_children(object: &Object) -> Box<dyn Iterator<Item = &Object> + '_> {
    match object {
        Object::Dictionary(value) => Box::new(value.iter().map(|(_, object)| object)),
        Object::Stream(value) => Box::new(value.dict.iter().map(|(_, object)| object)),
        Object::Array(value) => Box::new(value.iter()),
        _ => Box::new(std::iter::empty()),
    }
}

fn contains_signature(object: &Object, depth: usize) -> Result<bool, Error> {
    if depth >= MAX_DEPTH {
        return Err(unsupported("PDF nesting exceeds the limit"));
    }
    if let Object::Dictionary(dictionary) = object
        && (dictionary.has(b"ByteRange")
            || dictionary
                .get(b"Type")
                .and_then(Object::as_name)
                .is_ok_and(|name| matches!(name, b"Sig" | b"DocTimeStamp")))
    {
        return Ok(true);
    }
    for child in direct_children(object) {
        if contains_signature(child, depth + 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn metadata_references(
    document: &Document,
    object: &Object,
    objects: &mut HashSet<ObjectId>,
    depth: usize,
) -> Result<(), Error> {
    if depth >= MAX_DEPTH {
        return Err(unsupported(
            "PDF metadata reference depth exceeds the limit",
        ));
    }
    if let Object::Reference(id) = object {
        if objects.insert(*id) {
            let target = document
                .objects
                .get(id)
                .ok_or_else(|| unsupported("PDF metadata reference is invalid"))?;
            metadata_references(document, target, objects, depth + 1)?;
        }
    } else {
        for child in direct_children(object) {
            metadata_references(document, child, objects, depth + 1)?;
        }
    }
    Ok(())
}

pub(super) fn clean(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut document = load(bytes)?;
    if inspect_loaded(&document)?.1 {
        return Err(unsupported("PDF/A metadata must be preserved"));
    }
    let catalog = document
        .catalog()
        .map_err(|_| unsupported("PDF catalog is missing"))?;
    if catalog.has(b"Perms") {
        return Err(unsupported("Signed PDF"));
    }
    for object in document.objects.values() {
        if contains_signature(object, 0)? {
            return Err(unsupported("Signed PDF"));
        }
    }

    let mut targets = HashSet::new();
    for object in [document.trailer.get(b"Info"), catalog.get(b"Metadata")]
        .into_iter()
        .flatten()
    {
        metadata_references(&document, object, &mut targets, 0)?;
    }
    document.trailer.remove(b"Info");
    document
        .catalog_mut()
        .map_err(|_| unsupported("PDF catalog is missing"))?
        .remove(b"Metadata");
    document.prune_objects();
    if targets.iter().any(|id| document.objects.contains_key(id)) {
        return Err(unsupported(
            "PDF metadata is referenced by preserved content",
        ));
    }
    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|_| unsupported("Could not write PDF copy"))?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Stream, dictionary};

    fn fixture(xmp: &[u8], shared: bool) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages = document.new_object_id();
        let info = document
            .add_object(dictionary! { "Author" => Object::string_literal("Synthetic Author") });
        let metadata = document.add_object(Stream::new(
            dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
            xmp.to_vec(),
        ));
        let contents = document.add_object(Stream::new(Dictionary::new(), b"q Q".to_vec()));
        let link = document.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Link",
            "Rect" => vec![0.into(), 0.into(), 20.into(), 20.into()],
            "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com") },
        });
        let field = document.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx",
            "T" => Object::string_literal("Name"), "V" => Object::string_literal("Sample"),
            "Rect" => vec![0.into(), 25.into(), 40.into(), 40.into()],
        });
        let page = document.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages, "Contents" => contents,
            "Annots" => vec![Object::Reference(link), Object::Reference(field)],
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
            "Resources" => Dictionary::new(),
        });
        document.set_object(
            pages,
            dictionary! {
                "Type" => "Pages", "Count" => 1, "Kids" => vec![Object::Reference(page)],
            },
        );
        let mut catalog = dictionary! {
            "Type" => "Catalog", "Pages" => pages, "Metadata" => metadata,
            "AcroForm" => dictionary! { "Fields" => vec![Object::Reference(field)] },
        };
        if shared {
            catalog.set("Other", info);
        }
        let catalog = document.add_object(catalog);
        document.trailer.set("Root", catalog);
        document.trailer.set("Info", info);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn removes_pdf_properties_and_preserves_pages_links_and_forms() {
        let original = fixture(b"<root><title>Synthetic XMP title</title></root>", false);
        let found = inspect(&original).unwrap();
        assert!(
            found
                .details
                .iter()
                .any(|field| field.value == "Synthetic Author")
        );
        assert!(
            found
                .details
                .iter()
                .any(|field| field.value == "Synthetic XMP title")
        );

        let cleaned = clean(&original).unwrap();
        assert!(inspect(&cleaned).unwrap().passed());
        let previous = load(&original).unwrap();
        let copy = load(&cleaned).unwrap();
        assert_eq!(previous.get_pages(), copy.get_pages());
        let page = copy.get_pages()[&1];
        assert_eq!(previous.get_page_content(page), copy.get_page_content(page));
        assert_eq!(
            previous.get_page_annotations(page).unwrap(),
            copy.get_page_annotations(page).unwrap()
        );
        assert_eq!(
            previous.catalog().unwrap().get(b"AcroForm").unwrap(),
            copy.catalog().unwrap().get(b"AcroForm").unwrap()
        );
        for value in [b"Synthetic Author".as_slice(), b"Synthetic XMP title"] {
            assert!(!cleaned.windows(value.len()).any(|window| window == value));
        }
    }

    #[test]
    fn xmp_text_keeps_spaces_and_resolves_references() {
        let document = fixture(
            b"<rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><title><rdf:Alt><rdf:li>A &amp; B &#x21;<![CDATA[?]]></rdf:li></rdf:Alt></title></rdf:RDF>",
            false,
        );
        let result = inspect(&document).unwrap();
        assert!(
            result
                .details
                .iter()
                .any(|field| field.name == "title" && field.value == "A & B !?")
        );
    }

    #[test]
    fn full_rewrite_drops_metadata_from_prior_revisions() {
        let original = fixture(b"<root/>", false);
        let previous = load(&original).unwrap();
        let mut incremental = lopdf::IncrementalDocument::create_from(original, previous);
        let info = incremental.new_document.add_object(dictionary! {
            "Author" => Object::string_literal("Revised Author"),
        });
        incremental.new_document.trailer.set("Info", info);
        let mut input = Vec::new();
        incremental.save_to(&mut input).unwrap();
        assert!(
            inspect(&input)
                .unwrap()
                .details
                .iter()
                .any(|field| field.value == "Revised Author")
        );
        let output = clean(&input).unwrap();
        assert!(inspect(&output).unwrap().passed());
        assert!(
            !output
                .windows(b"Synthetic Author".len())
                .any(|value| value == b"Synthetic Author")
        );
        assert!(
            !output
                .windows(b"Revised Author".len())
                .any(|value| value == b"Revised Author")
        );
        assert_eq!(load(&output).unwrap().revision_count(), 1);
    }

    #[test]
    fn publishes_verified_copy_without_replacing_files() {
        use crate::core::{CleanMode, clean_with_options};
        use std::fs;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.pdf");
        let target = directory.path().join("clean.pdf");
        let original = fixture(b"<root/>", false);
        fs::write(&source, &original).unwrap();

        assert!(
            clean_with_options(&source, &target, CleanMode::Reencode, false)
                .unwrap()
                .passed()
        );
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(inspect(&fs::read(&target).unwrap()).unwrap().passed());
        fs::write(&target, b"existing").unwrap();
        assert!(matches!(
            clean_with_options(&source, &target, CleanMode::Reencode, false),
            Err(Error::OutputExists(_))
        ));
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        assert_eq!(fs::read(&source).unwrap(), original);
    }

    #[test]
    fn refuses_shared_properties_and_pdfa() {
        assert!(clean(&fixture(b"<root/>", true)).is_err());
        assert!(
            clean(&fixture(
                b"<pdfaid:part xmlns:pdfaid='http://www.aiim.org/pdfa/ns/id/'>1</pdfaid:part>",
                false
            ))
            .is_err()
        );
    }
}
