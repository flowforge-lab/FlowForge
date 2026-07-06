//! Client-side document text-extraction fallback (#338 follow-up).
//!
//! The OpenAI-compatible chat-completions wire and Ollama's native `/api/chat`
//! have no portable document block, so a `Document` attachment attached to one
//! of those providers is dropped at the capability strip (#338). The fallback
//! half benchmarked against OpenClaw: extract the document's text client-side
//! and fold it into the user message's prompt context, capped at
//! [`MAX_DOCS_PER_CALL`] documents per call. The native half (Bedrock
//! `DocumentBlock`) lives in the Bedrock adapter; this module is the opt-in
//! extraction path the OpenAI/Ollama adapters call when their connection's model
//! declares document support.
//!
//! Supported formats (PDF is deferred to a follow-up — see `DocFormat::Pdf`):
//! TXT / MD / CSV / JSON are lossy-decoded UTF-8; HTML is run through the same
//! `fast_html2md` rewriter `ff-tools` uses; DOCX / XLSX are cracked from their
//! OOXML zip containers with `quick-xml`. One unextractable document never drops
//! the whole turn — it degrades to an in-prompt note so the model can at least
//! acknowledge the attachment, mirroring the "skip rather than fail" discipline
//! used for unsupported image media types (#334).

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::attachment_bytes;
use crate::ChatMessage;
use ff_core::{Attachment, AttachmentKind};

/// Upper bound on document attachments carried into one provider call, matching
/// the OpenClaw benchmark. Documents beyond this in the request are noted as
/// skipped (oldest first) rather than extracted, so a multi-turn history that
/// accumulated more than this still leaves the most recent — and most
/// relevant — documents intact.
pub(crate) const MAX_DOCS_PER_CALL: usize = 10;

/// Upper bound on a single document's raw bytes before extraction is attempted.
/// Bounds the zip/xml parse work for a pathological upload; the per-document
/// extracted-text cap ([`MAX_EXTRACTED_TEXT_CHARS`]) bounds the *output* that
/// reaches the model. A document over this is skipped with a note.
pub(crate) const MAX_DOC_BYTES: u64 = 10 * 1024 * 1024;

/// Upper bound on the extracted text folded in for a single document. Matches
/// the `html_text::MAX_BYTES` convention used by `ff-tools::web_fetch`, keeping
/// one large document from flooding the model's context. Truncation is flagged
/// in the injected note so the model knows it is seeing a prefix.
pub(crate) const MAX_EXTRACTED_TEXT_CHARS: usize = 100_000;

/// A document format this module knows how to extract text from. `Pdf` and
/// `Other` (and the legacy binary `Doc`/`Xls`) are recognized-but-unsupported
/// and degrade to an in-prompt note rather than an extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocFormat {
    Text,
    Markdown,
    Csv,
    Json,
    Html,
    Docx,
    Xlsx,
    /// PDF text extraction is deferred to a follow-up (see module docs). A PDF
    /// attachment degrades to a note so the model can acknowledge it.
    Pdf,
    /// Legacy binary `.doc` (Word 97-2003) — not OOXML, not supported.
    Doc,
    /// Legacy binary `.xls` (Excel 97-2003) — not OOXML, not supported.
    Xls,
    /// Unrecognized media type and extension.
    Other,
}

impl DocFormat {
    /// Whether this format has a working text extractor in this module.
    fn extractable(self) -> bool {
        matches!(
            self,
            DocFormat::Text
                | DocFormat::Markdown
                | DocFormat::Csv
                | DocFormat::Json
                | DocFormat::Html
                | DocFormat::Docx
                | DocFormat::Xlsx
        )
    }
}

/// Resolve a document's format from its IANA media type, falling back to the
/// file-name extension (mirrors Bedrock's `document_format` discipline). The
/// caller already classified the attachment as a `Document`, so an unrecognized
/// type/extension yields `Other` (→ note) rather than a hard error.
fn doc_format(media_type: &str, name: Option<&str>) -> DocFormat {
    let by_media = match media_type.trim().to_ascii_lowercase().as_str() {
        "text/plain" => Some(DocFormat::Text),
        "text/markdown" => Some(DocFormat::Markdown),
        "text/csv" => Some(DocFormat::Csv),
        "text/html" => Some(DocFormat::Html),
        "application/json" | "text/json" => Some(DocFormat::Json),
        "application/pdf" => Some(DocFormat::Pdf),
        "application/msword" => Some(DocFormat::Doc),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(DocFormat::Docx)
        }
        "application/vnd.ms-excel" => Some(DocFormat::Xls),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            Some(DocFormat::Xlsx)
        }
        _ => None,
    };
    by_media.unwrap_or_else(|| {
        let ext = name
            .and_then(|n| n.rsplit_once('.'))
            .map(|(_, e)| e.to_ascii_lowercase());
        match ext.as_deref() {
            Some("txt") => DocFormat::Text,
            Some("md") | Some("markdown") => DocFormat::Markdown,
            Some("csv") => DocFormat::Csv,
            Some("html") | Some("htm") => DocFormat::Html,
            Some("json") => DocFormat::Json,
            Some("pdf") => DocFormat::Pdf,
            Some("doc") => DocFormat::Doc,
            Some("docx") => DocFormat::Docx,
            Some("xls") => DocFormat::Xls,
            Some("xlsx") => DocFormat::Xlsx,
            _ => DocFormat::Other,
        }
    })
}

/// Why a document's text could not be extracted. Carries a short reason that is
/// folded into the in-prompt note so the model (and ultimately the user) can see
/// *why* an attachment was skipped, never silently.
#[derive(Debug)]
enum ExtractError {
    /// PDF / legacy binary / unrecognized format (see [`DocFormat`]).
    UnsupportedFormat(DocFormat),
    /// The attachment's raw bytes exceed [`MAX_DOC_BYTES`].
    TooLarge { bytes: u64, limit: u64 },
    /// Reading the attachment's bytes failed (unreadable path, bad base64).
    Read(String),
    /// The format was recognized but its container/XML could not be parsed.
    Parse(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::UnsupportedFormat(fmt) => match fmt {
                DocFormat::Pdf => write!(f, "PDF text extraction is not yet supported"),
                DocFormat::Doc => write!(
                    f,
                    "legacy .doc (binary) is not supported — convert to .docx"
                ),
                DocFormat::Xls => write!(
                    f,
                    "legacy .xls (binary) is not supported — convert to .xlsx"
                ),
                DocFormat::Other => write!(f, "unsupported document format"),
                // Unreachable (these are extractable), but keep the arm total.
                other => write!(f, "unsupported document format ({other:?})"),
            },
            ExtractError::TooLarge { bytes, limit } => {
                write!(f, "document too large ({bytes} bytes; limit {limit})")
            }
            ExtractError::Read(e) => write!(f, "could not read attachment: {e}"),
            ExtractError::Parse(e) => write!(f, "could not parse document: {e}"),
        }
    }
}

/// Extract one document attachment's text, dispatching by format. The returned
/// text is uncapped — the caller ([`fold_documents_into_text`]) applies
/// [`MAX_EXTRACTED_TEXT_CHARS`] and wraps it in the in-prompt envelope.
fn extract_document_text(a: &Attachment) -> Result<String, ExtractError> {
    if a.bytes > MAX_DOC_BYTES {
        return Err(ExtractError::TooLarge {
            bytes: a.bytes,
            limit: MAX_DOC_BYTES,
        });
    }
    let format = doc_format(&a.media_type, a.name.as_deref());
    if !format.extractable() {
        return Err(ExtractError::UnsupportedFormat(format));
    }
    let bytes = attachment_bytes(a).map_err(ExtractError::Read)?;
    match format {
        DocFormat::Text | DocFormat::Markdown | DocFormat::Csv | DocFormat::Json => {
            Ok(String::from_utf8_lossy(&bytes).trim().to_string())
        }
        DocFormat::Html => Ok(html_to_text(&String::from_utf8_lossy(&bytes))),
        DocFormat::Docx => extract_docx(&bytes),
        DocFormat::Xlsx => extract_xlsx(&bytes),
        // Unreachable: `extractable` already rejected these. Kept total so a
        // future format addition can't fall through silently.
        DocFormat::Pdf | DocFormat::Doc | DocFormat::Xls | DocFormat::Other => {
            Err(ExtractError::UnsupportedFormat(format))
        }
    }
}

/// HTML → readable markdown text. Reuses `fast_html2md`'s lol_html rewriter (the
/// same engine `ff-tools::html_text` uses) so HTML attachments and `web_fetch`
/// produce consistent output. Script/style content is dropped by the converter;
/// surrounding whitespace is trimmed and runs of blank lines collapsed so a
/// page's chrome doesn't eat the model's context.
fn html_to_text(html: &str) -> String {
    // `fast_html2md`'s lib name is `html2md`.
    let md = html2md::rewrite_html(html, false);
    collapse_blank_lines(md.trim())
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0usize;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Extract paragraph-joined text from a `.docx` (OOXML) attachment. The document
/// body lives at `word/document.xml`; each `<w:p>` is a paragraph and each
/// `<w:t>` inside a run is a text node. We concatenate the `<w:t>` children and
/// insert a newline at every `</w:p>`, which preserves paragraph structure
/// (including table cells, which are paragraph-wrapped) without needing the
/// full styling/section model.
fn extract_docx(bytes: &[u8]) -> Result<String, ExtractError> {
    let xml = read_zip_entry(bytes, "word/document.xml")?;
    let body = std::str::from_utf8(&xml)
        .map_err(|e| ExtractError::Parse(format!("document.xml is not UTF-8: {e}")))?;
    let mut reader = Reader::from_str(body);
    // NOTE: trim_text is intentionally OFF. DOCX runs carry significant
    // inter-word spaces inside `<w:t>` ("Hello " + "world"); trimming would
    // collapse them to "Helloworld". Inter-element whitespace is ignored
    // anyway (only `in_t` text is appended).

    let mut out = String::new();
    let mut in_t = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if local_name(e.name().into_inner()) == b"t" {
                    in_t = true;
                }
            }
            Ok(Event::End(e)) => {
                let name = local_name(e.name().into_inner());
                if name == b"t" {
                    in_t = false;
                } else if name == b"p" {
                    // Paragraph boundary. Only emit a newline when the paragraph
                    // held text, so an empty styling paragraph doesn't add blanks.
                    if !out.ends_with('\n') && !out.is_empty() {
                        out.push('\n');
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                // A self-closed `<w:t/>` carries no text; a self-closed `<w:p/>`
                // is an empty paragraph — skip both.
                if local_name(e.name().into_inner()) == b"t" {
                    in_t = false;
                }
            }
            Ok(Event::Text(t)) if in_t => {
                if let Ok(s) = t.decode() {
                    out.push_str(&s);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(ExtractError::Parse(format!("docx xml: {e}"))),
        }
    }
    Ok(out.trim().to_string())
}

/// Extract a TSV-like text representation from an `.xlsx` (OOXML) attachment.
/// Shared strings live at `xl/sharedStrings.xml`; each worksheet at
/// `xl/worksheets/sheetN.xml` references them by index in `<c t="s"><v>i</v></c>`
/// or carries a literal in `<v>`. We emit one tab-joined row per `<row>`, one
/// newline per row, and a sheet header so the model can tell sheets apart.
fn extract_xlsx(bytes: &[u8]) -> Result<String, ExtractError> {
    let shared = read_shared_strings(bytes)?;
    let sheet_names = sheet_file_names(bytes);
    let mut out = String::new();
    for (i, sheet) in sheet_names.iter().enumerate() {
        let path = format!("xl/worksheets/{sheet}");
        let xml = match read_zip_entry(bytes, &path) {
            Ok(v) => v,
            Err(e) => {
                // A missing sheet file is non-fatal — note it and continue so a
                // partially-corrupt workbook still yields the other sheets.
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&format!("[sheet {i}: {e}]"));
                continue;
            }
        };
        let body = std::str::from_utf8(&xml)
            .map_err(|e| ExtractError::Parse(format!("{path} is not UTF-8: {e}")))?;
        let sheet_text = parse_sheet(body, &shared);
        if sheet_text.trim().is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("--- sheet {} ---\n{}", i + 1, sheet_text));
    }
    // Trim only newlines, NOT tabs/spaces: a trailing empty cell's tab is
    // significant column structure that `trim()` would erase.
    Ok(out
        .trim_start_matches(['\n', '\r'])
        .trim_end_matches(['\n', '\r'])
        .to_string())
}

/// Read `xl/sharedStrings.xml` into a list of strings, one per `<si>`.
fn read_shared_strings(bytes: &[u8]) -> Result<Vec<String>, ExtractError> {
    let xml = match read_zip_entry(bytes, "xl/sharedStrings.xml") {
        Ok(v) => v,
        // A workbook with no shared strings table (e.g. a numeric-only sheet)
        // is valid — empty list, cells fall back to their literal `<v>`.
        Err(_) => return Ok(Vec::new()),
    };
    let body = std::str::from_utf8(&xml)
        .map_err(|e| ExtractError::Parse(format!("sharedStrings.xml is not UTF-8: {e}")))?;
    let mut reader = Reader::from_str(body);
    // trim_text OFF: shared strings may carry significant leading/trailing
    // whitespace; inter-element whitespace is ignored (only `in_t` appended).

    let mut strings = Vec::new();
    let mut in_si = false;
    let mut in_t = false;
    let mut current = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match local_name(e.name().into_inner()) {
                b"si" => {
                    in_si = true;
                    current.clear();
                }
                b"t" if in_si => in_t = true,
                _ => {}
            },
            Ok(Event::End(e)) => match local_name(e.name().into_inner()) {
                b"si" => {
                    strings.push(std::mem::take(&mut current));
                    in_si = false;
                    in_t = false;
                }
                b"t" => in_t = false,
                _ => {}
            },
            Ok(Event::Empty(e)) if local_name(e.name().into_inner()) == b"t" => in_t = false,
            Ok(Event::Text(t)) if in_t => {
                if let Ok(s) = t.decode() {
                    current.push_str(&s);
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(ExtractError::Parse(format!("sharedStrings xml: {e}"))),
        }
    }
    Ok(strings)
}

/// Parse one worksheet body into tab/newline-joined rows, resolving shared
/// string indices against `shared`. Inline strings (`t="inlineStr"`) and literal
/// numbers/dates (`<v>`) are both handled; cells with neither yield an empty
/// field so the row's column structure is preserved.
fn parse_sheet(body: &str, shared: &[String]) -> String {
    let mut reader = Reader::from_str(body);
    // trim_text OFF: cell values may carry significant whitespace; inter-element
    // indentation is ignored (only `in_value` text is appended).

    let mut out = String::new();
    let mut in_row = false;
    let mut in_c = false;
    let mut in_v = false;
    let mut in_is = false;
    let mut in_is_t = false;
    let mut cell_shared = false;
    let mut row_cells: Vec<String> = Vec::new();
    let mut cell_value = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match local_name(e.name().into_inner()) {
                b"row" => {
                    in_row = true;
                    row_cells.clear();
                }
                b"c" => {
                    in_c = true;
                    cell_shared = has_attr(&e, b"t", b"s");
                    cell_value.clear();
                }
                b"v" if in_c => in_v = true,
                b"is" if in_c => in_is = true,
                b"t" if in_is => in_is_t = true,
                _ => {}
            },
            Ok(Event::End(e)) => match local_name(e.name().into_inner()) {
                b"row" => {
                    if in_row {
                        out.push_str(&row_cells.join("\t"));
                        out.push('\n');
                    }
                    in_row = false;
                }
                b"c" => {
                    if in_c {
                        row_cells.push(resolve_cell(&cell_value, cell_shared, shared));
                    }
                    in_c = false;
                    in_v = false;
                    in_is = false;
                    in_is_t = false;
                }
                b"v" => in_v = false,
                b"is" => in_is = false,
                b"t" if in_is => in_is_t = false,
                _ => {}
            },
            Ok(Event::Empty(e)) => {
                // An empty `<c/>` is a blank cell; record it so columns align.
                if local_name(e.name().into_inner()) == b"c" && in_row {
                    row_cells.push(String::new());
                }
            }
            Ok(Event::Text(t)) => {
                let in_value = in_c && (in_v || in_is_t);
                if in_value {
                    if let Ok(s) = t.decode() {
                        cell_value.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break, // a malformed sheet is non-fatal: return what we have
        }
    }
    // Strip only trailing newlines, NOT tabs: the last row's trailing tab (from
    // a trailing empty cell) is significant column structure, and `trim_end()`
    // would erase it.
    out.trim_end_matches(['\n', '\r']).to_string()
}

/// Resolve a cell's `<v>` payload against the shared-strings table. A shared
/// cell carries a numeric index; an inline cell carries the text directly; a
/// missing value is an empty cell.
fn resolve_cell(value: &str, shared: bool, strings: &[String]) -> String {
    if shared {
        value
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| strings.get(i).cloned())
            .unwrap_or_default()
    } else {
        value.to_string()
    }
}

/// Whether an element carries `attr="value"` (byte comparison, no namespace on
/// the attribute). Used to detect `t="s"` / `t="inlineStr"` on `<c>`.
fn has_attr(start: &quick_xml::events::BytesStart<'_>, attr: &[u8], value: &[u8]) -> bool {
    start.attributes().flatten().any(|a| {
        a.key.into_inner() == attr
            && a.unescape_value()
                .map(|v| v.as_bytes() == value)
                .unwrap_or(false)
    })
}

/// Read a single entry from a zip-backed byte slice. OOXML containers are
/// deflate-compressed; the `zip` crate is pinned to deflate-only for that
/// reason (see `Cargo.toml`).
///
/// # Zip-bomb hardening
///
/// The uncompressed size in the zip local-file/central-directory header is
/// attacker-controlled. A ~5 MB DOCX can declare a multi-GB uncompressed size,
/// and the naive `Vec::with_capacity(entry.size())` would abort the process on
/// allocation (or `read_to_end` would exhaust RAM). Two independent guards:
///
/// 1. **Declared-size guard**: reject when `entry.size()` exceeds
///    [`MAX_DOC_BYTES`], before any allocation. This is the primary vector — a
///    malicious header advertising a huge uncompressed size.
/// 2. **Read cap**: `take(MAX + 1)` hard-caps the *actual* bytes read regardless
///    of what the header says, so a header that *under*-reports the size (a
///    zip-bomb with mismatched headers, or a high-ratio deflate stream whose
///    declared size is a lie) cannot exhaust memory. The post-read length check
///    turns an over-cap actual into a [`ExtractError::TooLarge`] rather than an
///    unbounded buffer.
fn read_zip_entry(bytes: &[u8], name: &str) -> Result<Vec<u8>, ExtractError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| ExtractError::Parse(format!("zip open: {e}")))?;
    let entry = archive
        .by_name(name)
        .map_err(|e| ExtractError::Parse(format!("zip entry {name:?}: {e}")))?;
    // Guard 1: never trust the header's declared uncompressed size for
    // allocation. Reject early when it exceeds the cap.
    let declared = entry.size();
    if declared > MAX_DOC_BYTES {
        return Err(ExtractError::TooLarge {
            bytes: declared,
            limit: MAX_DOC_BYTES,
        });
    }
    // Guard 2: hard-cap the actual read. `take(MAX + 1)` lets us distinguish
    // "fit" (<= MAX) from "over" (== MAX + 1) after the read, even when the zip
    // impl ignores the header size field or the header under-reports.
    let mut buf = Vec::with_capacity(declared.min(MAX_DOC_BYTES) as usize);
    entry
        .take(MAX_DOC_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| ExtractError::Parse(format!("zip read {name:?}: {e}")))?;
    if buf.len() as u64 > MAX_DOC_BYTES {
        return Err(ExtractError::TooLarge {
            bytes: buf.len() as u64,
            limit: MAX_DOC_BYTES,
        });
    }
    Ok(buf)
}

/// List `xl/worksheets/sheetN.xml` entries in numeric order. Sheet file names
/// are `sheet1.xml`, `sheet2.xml`, … — sorted lexicographically on the number
/// so `sheet10` doesn't sort ahead of `sheet2`.
fn sheet_file_names(bytes: &[u8]) -> Vec<String> {
    let mut archive = match zip::ZipArchive::new(Cursor::new(bytes)) {
        Ok(a) => a,
        Err(_) => return Vec::new(),
    };
    let mut nums: Vec<u64> = Vec::new();
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_string();
        if let Some(rest) = name
            .strip_prefix("xl/worksheets/sheet")
            .and_then(|s| s.strip_suffix(".xml"))
        {
            if let Ok(n) = rest.parse::<u64>() {
                nums.push(n);
            }
        }
    }
    nums.sort_unstable();
    nums.into_iter().map(|n| format!("sheet{n}.xml")).collect()
}

/// The local (namespace-stripped) name of a tag, e.g. `w:t` → `t`. Uses
/// `rposition` rather than `slice::rsplit_once` (unstable) so this stays on
/// stable Rust.
fn local_name(tag: &[u8]) -> &[u8] {
    match tag.iter().rposition(|&b| b == b':') {
        Some(i) => &tag[i + 1..],
        None => tag,
    }
}

/// Cap `text` to [`MAX_EXTRACTED_TEXT_CHARS`] code points, never splitting a
/// char boundary. Returns the (possibly shortened) text and whether truncation
/// occurred. The caller ([`fold_one`]) owns the truncation sentinel — this
/// function only truncates and signals, so there is exactly one notice per
/// truncated document, never two.
fn cap_extracted_text(text: &str) -> (String, bool) {
    if text.chars().nth(MAX_EXTRACTED_TEXT_CHARS).is_none() {
        return (text.to_string(), false);
    }
    (text.chars().take(MAX_EXTRACTED_TEXT_CHARS).collect(), true)
}

/// Fold document attachments' extracted text into each message's `content`,
/// returning a new message list with documents replaced by their in-prompt text
/// and only image attachments retained. Called by the OpenAI/Ollama adapters
/// after the capability strip (so docs only reach here when the connection
/// declared document support). Enforces the per-call count and per-doc size
/// limits; one unextractable document degrades to a note rather than failing
/// the turn.
///
/// This is the opt-in path: the default OpenAI/Ollama provider keeps
/// `supports_documents = false`, so `messages_for_wire` strips docs before they
/// ever reach this function (#338 skip). Extraction runs only when the host
/// sets `with_documents(true)`.
pub(crate) fn fold_documents_into_text(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let total_docs: usize = messages
        .iter()
        .map(|m| {
            m.attachments
                .iter()
                .filter(|a| a.kind == AttachmentKind::Document)
                .count()
        })
        .sum();
    // Keep the most-recent [`MAX_DOCS_PER_CALL`] documents; the oldest
    // `total - MAX` (counting in document order across the history) are noted
    // as skipped so the model knows they existed but weren't read.
    let skip_leading = total_docs.saturating_sub(MAX_DOCS_PER_CALL);
    let mut seen = 0usize;

    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let has_docs = m
            .attachments
            .iter()
            .any(|a| a.kind == AttachmentKind::Document);
        if !has_docs {
            out.push(m.clone());
            continue;
        }
        let mut cloned = m.clone();
        let mut addition = String::new();
        // Take the attachments out so we can iterate by value while rebuilding
        // the list with only images kept.
        let mut images: Vec<ff_core::Attachment> = Vec::new();
        for a in std::mem::take(&mut cloned.attachments) {
            match a.kind {
                AttachmentKind::Document => {
                    seen += 1;
                    if seen <= skip_leading {
                        addition.push_str(&format_doc_note(
                            &a,
                            &format!(
                                "skipped: per-call limit of {MAX_DOCS_PER_CALL} documents reached"
                            ),
                        ));
                    } else {
                        addition.push_str(&fold_one(&a));
                    }
                }
                AttachmentKind::Image => images.push(a),
            }
        }
        cloned.attachments = images;
        if !addition.is_empty() {
            match &mut cloned.content {
                Some(c) if !c.is_empty() => {
                    c.push_str("\n\n");
                    c.push_str(&addition);
                }
                _ => {
                    cloned.content = Some(addition.trim().to_string());
                }
            }
        }
        out.push(cloned);
    }
    out
}

/// Extract one document and wrap it in the in-prompt envelope, or — on any
/// failure — substitute an explicit note. Either way the model sees that a
/// document was attached and either its text or the reason it was skipped.
fn fold_one(a: &Attachment) -> String {
    let label = doc_label(a);
    match extract_document_text(a) {
        Ok(text) => {
            let (text, truncated) = cap_extracted_text(&text);
            let truncation = if truncated {
                "\n[content truncated]"
            } else {
                ""
            };
            format!(
                "<document name=\"{label}\">\n<content>\n{text}\n</content>{truncation}\n</document>"
            )
        }
        Err(e) => format_doc_note(a, &e.to_string()),
    }
}

/// The user-facing name for an attachment, falling back to its media type when
/// no file name is known. Used in the in-prompt envelope so the model can refer
/// to the document by name.
fn doc_label(a: &Attachment) -> String {
    a.name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| a.media_type.clone())
}

/// An envelope for a document that could not be extracted, carrying the reason.
/// Kept in the same `<document>` shape as a successful extraction so the model
/// treats it uniformly.
fn format_doc_note(a: &Attachment, reason: &str) -> String {
    let label = doc_label(a);
    format!("<document name=\"{label}\">\n[extraction skipped: {reason}]\n</document>")
}

#[cfg(test)]
mod tests;
