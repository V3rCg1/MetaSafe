use std::{fs, io::Write};

use lopdf::{Dictionary, Object, ObjectStream, Stream, Writer, dictionary};
use metasafe::core::{self, CleanMode};

// Build real compressed members and an xref stream without depending on the
// document writer's choice of how many members to group into each stream.
fn pdf_with_object_streams(count: usize, payload_size: usize, layers: usize) -> Vec<u8> {
    let mut objects = vec![
        Object::Dictionary(dictionary! { "Type" => "Catalog", "Pages" => (2, 0),
        "Payloads" => (0..count).map(|i| Object::Reference((100 + i as u32, 0))).collect::<Vec<_>>() }),
        Object::Dictionary(dictionary! { "Type" => "Pages", "Count" => 1,
        "Kids" => vec![Object::Reference((3, 0))] }),
        Object::Dictionary(dictionary! { "Type" => "Page", "Parent" => (2, 0),
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        "Resources" => Dictionary::new() }),
    ];
    for index in 0..count {
        let mut stream = ObjectStream::builder().compression_level(0).build();
        stream
            .add_object(
                (100 + index as u32, 0),
                Object::string_literal(vec![b'A'; payload_size]),
            )
            .unwrap();
        let mut stream = stream.to_stream_object().unwrap();
        for _ in 0..layers {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&stream.content).unwrap();
            stream.set_content(encoder.finish().unwrap());
        }
        stream.dict.set(
            "Filter",
            Object::Array(vec![Object::Name(b"FlateDecode".to_vec()); layers]),
        );
        objects.push(Object::Stream(stream));
    }
    let members: Vec<_> = (0..count)
        .map(|index| (100 + index as u32, 4 + index as u32, 0))
        .collect();
    serialize_pdf(&objects, &members)
}

fn serialize_pdf(objects: &[Object], members: &[(u32, u32, u16)]) -> Vec<u8> {
    serialize_pdf_with_xref_filter(objects, members, true)
}

fn serialize_pdf_with_xref_filter(
    objects: &[Object],
    members: &[(u32, u32, u16)],
    compress: bool,
) -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        writeln!(bytes, "{} 0 obj", index + 1).unwrap();
        Writer::write_object(&mut bytes, object).unwrap();
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref_id = objects.len() + 1;
    let xref_offset = bytes.len();
    offsets.push(xref_offset);
    let size = members
        .iter()
        .map(|&(id, _, _)| id as usize + 1)
        .max()
        .unwrap_or(0)
        .max(xref_id + 1);
    let mut entries = Vec::new();
    for id in 0..size {
        let (kind, field, generation): (u8, u32, u16) = if id == 0 {
            (0, 0, 65535)
        } else if id < offsets.len() {
            (1, offsets[id] as u32, 0)
        } else if let Some(&(_, container, index)) = members
            .iter()
            .find(|&&(number, _, _)| number as usize == id)
        {
            (2, container, index)
        } else {
            (0, 0, 0)
        };
        entries.push(kind);
        entries.extend_from_slice(&field.to_be_bytes());
        entries.extend_from_slice(&generation.to_be_bytes());
    }
    let mut xref = Stream::new(
        dictionary! { "Type" => "XRef", "Size" => size as i64,
        "Root" => (1, 0), "W" => vec![1.into(), 4.into(), 2.into()] },
        entries,
    );
    if compress {
        xref.compress().unwrap();
    } else {
        xref.dict.set("Filter", Object::Array(Vec::new()));
    }
    writeln!(bytes, "{xref_id} 0 obj").unwrap();
    Writer::write_object(&mut bytes, &Object::Stream(xref)).unwrap();
    write!(bytes, "\nendobj\nstartxref\n{xref_offset}\n%%EOF\n").unwrap();
    bytes
}

#[test]
fn empty_filter_arrays_preserve_plain_object_and_xref_streams() {
    let input = pdf_with_object_streams(1, 1024, 0);
    let document = load_with_budget(&input, 8192).unwrap();
    assert_eq!(
        document.objects[&(100, 0)].as_str().unwrap(),
        vec![b'A'; 1024]
    );
    let objects: Vec<_> = (1..=4)
        .map(|id| document.objects[&(id, 0)].clone())
        .collect();
    let plain_xref = serialize_pdf_with_xref_filter(&objects, &[(100, 4, 0)], false);
    let loaded = load_with_budget(&plain_xref, 8192).unwrap();
    assert_eq!(
        loaded.objects[&(100, 0)].as_str().unwrap(),
        vec![b'A'; 1024]
    );
    assert!(load_with_budget(&plain_xref, 1024).is_err());
}

#[test]
fn indirect_stream_length_cannot_bypass_budget_via_lazy_object_loading() {
    let mut container = ObjectStream::builder().build();
    container.add_object((100, 0), Object::Integer(3)).unwrap();
    container
        .add_object((101, 0), Object::string_literal(vec![b'A'; 8192]))
        .unwrap();
    let mut contents = Stream::new(Dictionary::new(), b"q Q".to_vec());
    let mut objects = vec![
        Object::Dictionary(
            dictionary! { "Type" => "Catalog", "Pages" => (2, 0), "Payload" => (101, 0) },
        ),
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Count" => 1, "Kids" => vec![Object::Reference((3, 0))] },
        ),
        Object::Dictionary(
            dictionary! { "Type" => "Page", "Parent" => (2, 0), "Contents" => (4, 0),
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()], "Resources" => Dictionary::new() },
        ),
        Object::Stream(contents.clone()),
        Object::Stream(container.to_stream_object().unwrap()),
    ];
    let direct = serialize_pdf(&objects, &[(100, 5, 0), (101, 5, 1)]);
    assert!(load_with_budget(&direct, 12 * 1024).is_ok());
    contents.dict.set("Length", Object::Reference((100, 0)));
    objects[3] = Object::Stream(contents);
    let indirect = serialize_pdf(&objects, &[(100, 5, 0), (101, 5, 1)]);
    // Resolving /Length loads the container once, then eager loading decodes it
    // again. Both decodes must use the same budget.
    assert!(load_with_budget(&indirect, 12 * 1024).is_err());
    let loaded = load_with_budget(&indirect, 24 * 1024).unwrap();
    assert_eq!(loaded.objects[&(4, 0)].as_stream().unwrap().content, b"q Q");
}

#[test]
fn previous_xref_revisions_share_the_same_budget() {
    let original = pdf_with_object_streams(1, 1024, 1);
    let previous = lopdf::Document::load_mem(&original).unwrap();
    let base_budget: usize = previous
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .map(|stream| stream.decompressed_content_with_limit(8192).unwrap().len())
        .sum();
    assert!(load_with_budget(&original, base_budget).is_ok());
    let mut incremental = lopdf::IncrementalDocument::create_from(original, previous);
    let info = incremental
        .new_document
        .add_object(dictionary! { "Author" => Object::string_literal("Synthetic") });
    incremental.new_document.trailer.set("Info", info);
    let mut updated = Vec::new();
    incremental.save_to(&mut updated).unwrap();
    assert!(load_with_budget(&updated, base_budget).is_err());
    let loaded = load_with_budget(&updated, base_budget + 8192).unwrap();
    assert_eq!(
        loaded.objects[&(100, 0)].as_str().unwrap(),
        vec![b'A'; 1024]
    );
    assert_eq!(loaded.revision_count(), 2);
}

#[test]
fn aggregate_pdf_budget_rejects_before_publishing_a_copy() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("aggregate.pdf");
    let destination = directory.path().join("copy.pdf");
    let original = pdf_with_object_streams(17, 4 * 1024 * 1024, 1);
    assert!(original.len() < 100_000);
    fs::write(&source, &original).unwrap();
    assert!(core::inspect(&source).is_err());
    assert!(core::verify(&source).is_err());
    assert!(core::clean_with_options(&source, &destination, CleanMode::Reencode, false).is_err());
    assert!(!destination.exists());
    assert_eq!(fs::read(&source).unwrap(), original);
}

fn load_with_budget(bytes: &[u8], budget: usize) -> lopdf::Result<lopdf::Document> {
    lopdf::Document::load_mem_with_options(
        bytes,
        lopdf::LoadOptions {
            strict: true,
            max_decompressed_size: Some(8 * 1024 * 1024),
            max_total_decompressed_size: Some(budget),
            ..Default::default()
        },
    )
}

#[test]
fn aggregate_budget_covers_xref_and_encoded_and_plain_object_streams() {
    for layers in [0, 1, 2] {
        let input = pdf_with_object_streams(3, 8192, layers);
        // Each member fits, but together they exceed the shared budget.
        assert!(
            load_with_budget(&input, 16 * 1024).is_err(),
            "layers={layers}"
        );
        assert!(
            load_with_budget(&input, 32 * 1024).is_ok(),
            "layers={layers}"
        );
        // The xref stream alone exceeds this budget, before any object filter.
        assert!(load_with_budget(&input, 1).is_err());
    }
}

#[test]
fn budget_is_per_load_and_charges_intermediate_filter_output() {
    let input = pdf_with_object_streams(1, 8192, 2);
    let unrestricted = lopdf::Document::load_mem(&input).unwrap();
    let final_bytes: usize = unrestricted
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .map(|stream| {
            stream
                .decompressed_content_with_limit(32 * 1024)
                .unwrap()
                .len()
        })
        .sum();
    // A budget that covers the final bytes of every stream must still reject
    // the nested encoding, because the intermediate filter output also counts.
    assert!(load_with_budget(&pdf_with_object_streams(1, 8192, 1), final_bytes).is_ok());
    assert!(load_with_budget(&input, final_bytes).is_err());
    for _ in 0..2 {
        let document = load_with_budget(&input, 16 * 1024).unwrap();
        assert_eq!(document.objects[&(100, 0)].as_str().unwrap().len(), 8192);
        assert_eq!(document.get_pages().len(), 1);
    }
}

#[test]
fn ordinary_pdf_still_cleans_and_preserves_compressed_members() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("ordinary.pdf");
    let destination = directory.path().join("copy.pdf");
    let original = pdf_with_object_streams(3, 8192, 1);
    fs::write(&source, &original).unwrap();
    assert!(core::inspect(&source).unwrap().passed());
    assert!(core::clean(&source, &destination).unwrap().passed());
    let copy = load_with_budget(&fs::read(&destination).unwrap(), 32 * 1024).unwrap();
    for index in 0..3 {
        assert_eq!(
            copy.objects[&(100 + index, 0)].as_str().unwrap(),
            vec![b'A'; 8192]
        );
    }
    assert_eq!(fs::read(&source).unwrap(), original);
}
