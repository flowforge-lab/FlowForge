use super::*;
use base64::Engine as _;
use ff_core::AttachmentSource;
use std::io::Cursor;
use std::io::Write;

fn doc(media_type: &str, name: Option<&str>, bytes: &[u8]) -> Attachment {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Attachment {
        kind: AttachmentKind::Document,
        media_type: media_type.into(),
        source: AttachmentSource::Inline(b64),
        name: name.map(str::to_string),
        bytes: bytes.len() as u64,
    }
}

fn msg(content: &str, atts: Vec<Attachment>) -> ChatMessage {
    ChatMessage::multimodal("user", content, atts)
}

// --- format dispatch ---

#[test]
fn doc_format_maps_known_media_types() {
    assert_eq!(doc_format("text/plain", None), DocFormat::Text);
    assert_eq!(doc_format("TEXT/MARKDOWN", None), DocFormat::Markdown);
    assert_eq!(doc_format("text/csv", None), DocFormat::Csv);
    assert_eq!(doc_format("text/html", None), DocFormat::Html);
    assert_eq!(doc_format("application/json", None), DocFormat::Json);
    assert_eq!(doc_format("application/pdf", None), DocFormat::Pdf);
    assert_eq!(
        doc_format(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            None,
        ),
        DocFormat::Docx,
    );
    assert_eq!(
        doc_format(
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            None,
        ),
        DocFormat::Xlsx,
    );
    // Legacy binary formats are recognized but unsupported.
    assert_eq!(doc_format("application/msword", None), DocFormat::Doc);
    assert_eq!(doc_format("application/vnd.ms-excel", None), DocFormat::Xls);
}

#[test]
fn doc_format_falls_back_to_extension() {
    // An empty/unknown media type still routes via the file extension.
    assert_eq!(
        doc_format("application/octet-stream", Some("r.docx")),
        DocFormat::Docx
    );
    assert_eq!(doc_format("", Some("notes.md")), DocFormat::Markdown);
    assert_eq!(doc_format("", Some("data.xlsx")), DocFormat::Xlsx);
    assert_eq!(doc_format("", Some("book.pdf")), DocFormat::Pdf);
    assert_eq!(doc_format("", Some("unknown.xyz")), DocFormat::Other);
}

// --- text-based formats ---

#[test]
fn extracts_text_attachment_lossily() {
    let a = doc("text/plain", Some("notes.txt"), b"hello world");
    assert_eq!(extract_document_text(&a).unwrap(), "hello world");
}

#[test]
fn extracts_markdown_attachment_verbatim() {
    let body = "# Title\n\nsome **bold** text\n";
    let a = doc("text/markdown", Some("r.md"), body.as_bytes());
    assert_eq!(extract_document_text(&a).unwrap(), body.trim());
}

#[test]
fn extracts_csv_attachment_verbatim() {
    let body = "a,b,c\n1,2,3\n";
    let a = doc("text/csv", Some("d.csv"), body.as_bytes());
    assert_eq!(extract_document_text(&a).unwrap(), body.trim());
}

#[test]
fn extracts_json_attachment_verbatim() {
    let body = r#"{"k":"v"}"#;
    let a = doc("application/json", Some("d.json"), body.as_bytes());
    assert_eq!(extract_document_text(&a).unwrap(), body);
}

#[test]
fn extracts_html_attachment_as_markdown() {
    let html = r#"<html><body><h1>Title</h1><p>Hello <b>world</b>.</p>
        <script>alert('x')</script></body></html>"#;
    let a = doc("text/html", Some("p.html"), html.as_bytes());
    let md = extract_document_text(&a).unwrap();
    assert!(md.contains("Title"), "{md}");
    assert!(md.contains("Hello"), "{md}");
    assert!(!md.contains("alert("), "script body stripped: {md}");
}

// --- PDF / legacy / unsupported degrade to a note ---

#[test]
fn pdf_degrades_to_unsupported_format() {
    let a = doc("application/pdf", Some("r.pdf"), b"%PDF-1.4");
    let err = extract_document_text(&a).unwrap_err();
    assert!(
        err.to_string().contains("PDF"),
        "PDF unsupported note should name PDF: {err}"
    );
}

#[test]
fn legacy_doc_degrades_to_unsupported_format() {
    let a = doc("application/msword", Some("r.doc"), b"\xd0\xcf\x11\xe0");
    let err = extract_document_text(&a).unwrap_err();
    assert!(err.to_string().contains(".doc"), "{err}");
}

#[test]
fn unknown_format_degrades() {
    let a = doc("application/octet-stream", Some("x.xyz"), b"whatever");
    assert!(matches!(
        extract_document_text(&a).unwrap_err(),
        ExtractError::UnsupportedFormat(DocFormat::Other)
    ));
}

#[test]
fn oversized_document_degrades_to_too_large() {
    let a = Attachment {
        kind: AttachmentKind::Document,
        media_type: "text/plain".into(),
        source: AttachmentSource::Inline("aGk=".into()),
        name: Some("huge.txt".into()),
        bytes: MAX_DOC_BYTES + 1,
    };
    let err = extract_document_text(&a).unwrap_err();
    assert!(err.to_string().contains("too large"), "{err}");
}

// --- DOCX extraction ---

/// Build a minimal `.docx` zip in memory with the given `word/document.xml`
/// body. DOCX entries are deflate-compressed; `zip` is configured for
/// deflate, so the writer uses it by default.
fn docx_zip(document_xml: &str) -> Vec<u8> {
    let buf = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("word/document.xml", opts).unwrap();
    zip.write_all(document_xml.as_bytes()).unwrap();
    let out = zip.finish().unwrap();
    out.into_inner()
}

#[test]
fn extracts_docx_paragraphs_and_runs() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="w"><w:body>
<w:p><w:r><w:t>Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p>
<w:p><w:r><w:t>Second paragraph</w:t></w:r></w:p>
</w:body></w:document>"#;
    let bytes = docx_zip(xml);
    let a = doc(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("r.docx"),
        &bytes,
    );
    let text = extract_document_text(&a).unwrap();
    assert!(
        text.contains("Hello world"),
        "runs joined within a paragraph: {text}"
    );
    assert!(text.contains("Second paragraph"), "{text}");
    assert!(
        text.lines().any(|l| l == "Hello world"),
        "paragraphs separated by newlines: {text}"
    );
}

#[test]
fn empty_docx_degrades_cleanly() {
    let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="w"><w:body></w:body></w:document>"#;
    let bytes = docx_zip(xml);
    let a = doc(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("empty.docx"),
        &bytes,
    );
    assert_eq!(extract_document_text(&a).unwrap(), "");
}

#[test]
fn corrupt_docx_bytes_degrade_to_parse_error() {
    // Raw non-zip bytes passed off as a DOCX hit the "zip open" error branch
    // before any entry is read. CONTRIBUTING requires edge-case tests for new
    // modules; this guards the non-zip-input path that the happy-path DOCX
    // tests don't exercise.
    let a = doc(
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("not-a-docx.docx"),
        b"not a zip",
    );
    let err = extract_document_text(&a).unwrap_err();
    assert!(
        matches!(err, ExtractError::Parse(ref m) if m.contains("zip open")),
        "expected a Parse error from the zip-open failure, got {err}"
    );
}

// --- XLSX extraction ---

/// Build a minimal `.xlsx` zip with a shared-strings table and one sheet.
fn xlsx_zip(shared_strings: Option<&str>, sheet: &str) -> Vec<u8> {
    let buf = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buf);
    let opts = zip::write::SimpleFileOptions::default();
    if let Some(ss) = shared_strings {
        zip.start_file("xl/sharedStrings.xml", opts).unwrap();
        zip.write_all(ss.as_bytes()).unwrap();
    }
    zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
    zip.write_all(sheet.as_bytes()).unwrap();
    let out = zip.finish().unwrap();
    out.into_inner()
}

#[test]
fn extracts_xlsx_shared_strings_and_literals() {
    let ss = r#"<?xml version="1.0"?>
<sst xmlns="x"><si><t>Alpha</t></si><si><t>Beta</t></si></sst>"#;
    // Row 1: shared(0)="Alpha", literal 42. Row 2: shared(1)="Beta", empty.
    let sheet = r#"<?xml version="1.0"?>
<worksheet xmlns="x"><sheetData>
<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42</v></c></row>
<row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2"/></row>
</sheetData></worksheet>"#;
    let bytes = xlsx_zip(Some(ss), sheet);
    let a = doc(
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("d.xlsx"),
        &bytes,
    );
    let text = extract_document_text(&a).unwrap();
    assert!(
        text.contains("Alpha\t42"),
        "shared + literal in a row: {text}"
    );
    assert!(text.contains("Beta\t"), "shared + empty cell: {text}");
    assert!(
        text.contains("--- sheet 1 ---"),
        "sheet header emitted: {text}"
    );
}

#[test]
fn xlsx_without_shared_strings_still_emits_literals() {
    // A numeric-only workbook has no sharedStrings.xml; cells fall back to
    // their literal <v>.
    let sheet = r#"<?xml version="1.0"?>
<worksheet xmlns="x"><sheetData>
<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c></row>
</sheetData></worksheet>"#;
    let bytes = xlsx_zip(None, sheet);
    let a = doc(
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("nums.xlsx"),
        &bytes,
    );
    let text = extract_document_text(&a).unwrap();
    assert!(text.contains("1\t2"), "literal cells: {text}");
}

// --- fold_documents_into_text ---

#[test]
fn fold_appends_extracted_text_to_user_content() {
    let a = doc("text/plain", Some("notes.txt"), b"hello world");
    let m = msg("summarize this", vec![a]);
    let out = fold_documents_into_text(std::slice::from_ref(&m));
    assert_eq!(out.len(), 1);
    // The doc attachment is gone; its text is folded into content.
    assert!(
        out[0].attachments.is_empty(),
        "doc attachment dropped after fold"
    );
    let c = out[0].content.as_deref().unwrap();
    assert!(c.contains("summarize this"), "original text preserved: {c}");
    assert!(c.contains("hello world"), "extracted text folded in: {c}");
    assert!(c.contains("notes.txt"), "document name in envelope: {c}");
    assert!(c.contains("<document"), "envelope emitted: {c}");
    assert!(c.contains("</document>"), "envelope closed: {c}");
}

#[test]
fn fold_preserves_image_attachments_alongside_documents() {
    let img = ff_core::Attachment {
        kind: AttachmentKind::Image,
        media_type: "image/png".into(),
        source: AttachmentSource::Inline("aGk=".into()),
        name: Some("shot.png".into()),
        bytes: 2,
    };
    let d = doc("text/plain", Some("notes.txt"), b"hi");
    let m = msg("look", vec![img, d]);
    let out = fold_documents_into_text(std::slice::from_ref(&m));
    // Image kept, document folded into text.
    assert_eq!(out[0].attachments.len(), 1);
    assert_eq!(out[0].attachments[0].kind, AttachmentKind::Image);
    assert!(out[0].content.as_deref().unwrap().contains("hi"));
}

#[test]
fn fold_message_without_documents_is_cloned_unchanged() {
    let m = ChatMessage::text("user", "hi");
    let out = fold_documents_into_text(std::slice::from_ref(&m));
    assert_eq!(out[0].content.as_deref(), Some("hi"));
    assert!(out[0].attachments.is_empty());
}

#[test]
fn fold_over_limit_skips_oldest_and_notes_them() {
    // 12 text docs in a single message: the first 2 are skipped (keep last
    // 10), and each skipped doc is noted in the prompt.
    let atts: Vec<Attachment> = (0..12)
        .map(|i| {
            doc(
                "text/plain",
                Some(&format!("n{i}.txt")),
                format!("body {i}").as_bytes(),
            )
        })
        .collect();
    let m = msg("here", atts);
    let out = fold_documents_into_text(std::slice::from_ref(&m));
    let c = out[0].content.as_deref().unwrap();
    // The two oldest (n0, n1) are skipped: noted by name, content NOT extracted.
    assert!(c.contains("n0.txt"), "skipped doc named in its note: {c}");
    assert!(
        !c.contains("body 0"),
        "skipped doc's content is not extracted: {c}"
    );
    assert!(
        c.contains("skipped: per-call limit"),
        "skip reason present: {c}"
    );
    // The newest 10 are extracted; the 2 excess are noted as skipped.
    let extracted = c.matches("<content>").count();
    let skipped = c.matches("extraction skipped").count();
    assert_eq!(
        extracted, MAX_DOCS_PER_CALL,
        "exactly the limit is extracted"
    );
    assert_eq!(skipped, 2, "the excess (12 - 10) are noted as skipped");
}

#[test]
fn fold_failed_extraction_degrades_to_note_not_error() {
    // A PDF on the extraction path yields a note, not a dropped turn.
    let a = doc("application/pdf", Some("r.pdf"), b"%PDF-1.4");
    let m = msg("summarize", vec![a]);
    let out = fold_documents_into_text(std::slice::from_ref(&m));
    let c = out[0].content.as_deref().unwrap();
    assert!(c.contains("r.pdf"), "the attachment is named: {c}");
    assert!(c.contains("extraction skipped"), "degrades to a note: {c}");
    assert!(c.contains("PDF"), "reason names PDF: {c}");
}

#[test]
fn fold_truncates_oversized_extracted_text() {
    // A text doc whose extracted text exceeds the cap is truncated and flagged.
    let huge = "x".repeat(MAX_EXTRACTED_TEXT_CHARS + 500);
    let a = doc("text/plain", Some("big.txt"), huge.as_bytes());
    let m = msg("read", vec![a]);
    let out = fold_documents_into_text(std::slice::from_ref(&m));
    let c = out[0].content.as_deref().unwrap();
    assert!(c.contains("[content truncated]"), "truncation flagged: {c}");
    // Regression: exactly ONE truncation notice — `cap_extracted_text` used
    // to also append a broken inner literal, so a truncated doc got two
    // sentinels (and the inner one leaked "{MAX_EXTRACTED_TEXT_CHARS}").
    assert_eq!(
        c.matches("[content truncated").count(),
        1,
        "exactly one truncation sentinel, not two: {c}"
    );
    assert!(
        !c.contains("{MAX_EXTRACTED_TEXT_CHARS}"),
        "the const name must never leak as a literal into the prompt: {c}"
    );
    // The folded content stays bounded: the prefix plus the envelope overhead.
    assert!(
        c.len() < huge.len() + 1024,
        "folded content must not balloon past the cap + overhead: {}",
        c.len()
    );
}

#[test]
fn cap_extracted_text_is_a_noop_under_the_limit() {
    let (out, truncated) = cap_extracted_text("short");
    assert_eq!(out, "short");
    assert!(!truncated);
}

#[test]
fn cap_extracted_text_truncates_and_signals_without_a_sentinel() {
    // `cap_extracted_text` truncates to the cap and signals via the bool,
    // but emits NO sentinel itself — `fold_one` owns the (single) notice.
    // This guards against re-introducing the broken inner literal that
    // leaked "{MAX_EXTRACTED_TEXT_CHARS}" verbatim into the model context.
    let huge = "x".repeat(MAX_EXTRACTED_TEXT_CHARS + 10);
    let (out, truncated) = cap_extracted_text(&huge);
    assert!(truncated, "over-cap text is flagged");
    assert_eq!(
        out.chars().count(),
        MAX_EXTRACTED_TEXT_CHARS,
        "output is exactly the cap"
    );
    assert!(
        !out.contains("truncat"),
        "cap_extracted_text must not emit its own sentinel: {out}"
    );
    assert!(
        !out.contains("{MAX_EXTRACTED_TEXT_CHARS}"),
        "the const name must never appear in the returned string: {out}"
    );
}

// --- zip-bomb hardening (CVE-style: declared uncompressed size is
// attacker-controlled; `read_zip_entry` must reject an oversized declared
// size from the header alone, before any allocation or read) ---

/// Build a minimal single-entry zip whose headers declare
/// `declared_uncompressed` as the uncompressed size, but whose actual stored
/// data is `data` (tiny). The compressed size in the headers matches
/// `data.len()` (so the `zip` crate can locate the data section), while the
/// uncompressed size is the attacker's lie. Used to prove the declared-size
/// guard rejects an oversized entry from the header alone, before any
/// allocation or read. Assumes `declared_uncompressed` fits in a u32
/// (classic zip, non-zip64) — true for `MAX_DOC_BYTES + 1` (~10 MB).
fn malicious_zip(name: &str, declared_uncompressed: u64, data: &[u8]) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len() as u16;
    let data_len = data.len() as u32;
    let declared = declared_uncompressed as u32;
    let mut out = Vec::new();

    // Local file header (signature PK\x03\x04).
    out.extend_from_slice(&0x04034b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed: 2.0
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&0u16.to_le_bytes()); // compression: stored
    out.extend_from_slice(&0u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0u16.to_le_bytes()); // mod date
    out.extend_from_slice(&0u32.to_le_bytes()); // crc32 (not validated on open)
    out.extend_from_slice(&data_len.to_le_bytes()); // compressed size (actual)
    out.extend_from_slice(&declared.to_le_bytes()); // uncompressed size (the lie)
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra len
    out.extend_from_slice(name_bytes);
    out.extend_from_slice(data);

    let cd_offset = out.len() as u32;

    // Central directory file header (signature PK\x01\x02).
    out.extend_from_slice(&0x02014b50u32.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes()); // version made by
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&0u16.to_le_bytes()); // compression: stored
    out.extend_from_slice(&0u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0u16.to_le_bytes()); // mod date
    out.extend_from_slice(&0u32.to_le_bytes()); // crc32
    out.extend_from_slice(&data_len.to_le_bytes()); // compressed size (actual)
    out.extend_from_slice(&declared.to_le_bytes()); // uncompressed size (the lie)
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra len
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number start
    out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
    out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
    out.extend_from_slice(&0u32.to_le_bytes()); // local header offset: 0
    out.extend_from_slice(name_bytes);

    let cd_size = (out.len() as u32) - cd_offset;

    // End of central directory (signature PK\x05\x06).
    out.extend_from_slice(&0x06054b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with CD start
    out.extend_from_slice(&1u16.to_le_bytes()); // entries on this disk
    out.extend_from_slice(&1u16.to_le_bytes()); // total entries
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out
}

#[test]
fn oversized_uncompressed_entry_is_rejected_without_reading() {
    // A ~5 MB DOCX can declare a multi-GB uncompressed size in its local-file
    // header; the naive `Vec::with_capacity(entry.size())` would abort on
    // allocation. The declared-size guard must reject it from the header
    // alone, before any allocation or read.
    let zip = malicious_zip("word/document.xml", MAX_DOC_BYTES + 1, b"<w:p/>");
    let err = read_zip_entry(&zip, "word/document.xml").unwrap_err();
    match err {
        ExtractError::TooLarge { bytes, limit } => {
            assert_eq!(bytes, MAX_DOC_BYTES + 1, "reported the declared size");
            assert_eq!(limit, MAX_DOC_BYTES);
        }
        other => panic!("expected TooLarge from the declared-size guard, got {other}"),
    }
}

#[test]
fn oversized_docx_degrades_to_note_not_oom() {
    // End-to-end: an oversized declared document.xml degrades to an in-prompt
    // note via the fold, never OOMs. Proves the guard fires through the
    // public extraction path, not just at the read primitive.
    let bytes = malicious_zip(
        "word/document.xml",
        MAX_DOC_BYTES + 1,
        b"<?xml version=\"1.0\"?><w:document xmlns:w=\"w\"/>",
    );
    let a = Attachment {
        kind: AttachmentKind::Document,
        media_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            .into(),
        source: AttachmentSource::Inline(base64::engine::general_purpose::STANDARD.encode(&bytes)),
        name: Some("bomb.docx".into()),
        bytes: bytes.len() as u64, // compressed input is tiny; the lie is the uncompressed size
    };
    let m = msg("summarize", vec![a]);
    let out = fold_documents_into_text(std::slice::from_ref(&m));
    let c = out[0].content.as_deref().unwrap();
    assert!(c.contains("bomb.docx"), "the attachment is named: {c}");
    assert!(
        c.contains("extraction skipped"),
        "oversized entry degrades to a note: {c}"
    );
    assert!(
        c.contains("too large"),
        "the reason names the size violation: {c}"
    );
}

#[test]
fn read_zip_entry_still_succeeds_for_a_legit_small_entry() {
    // Regression: the guards must not reject a well-formed, under-cap entry.
    // A normal small DOCX (built via the `zip` writer) still reads back fine.
    let xml = r#"<?xml version="1.0"?>
<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>ok</w:t></w:r></w:p></w:body></w:document>"#;
    let bytes = docx_zip(xml);
    let out = read_zip_entry(&bytes, "word/document.xml").unwrap();
    assert!(String::from_utf8_lossy(&out).contains("ok"));
}
