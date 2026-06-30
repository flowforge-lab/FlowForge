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
mod tests {
    use super::*;
    use base64::Engine as _;
    use ff_core::AttachmentSource;
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
            source: AttachmentSource::Inline(
                base64::engine::general_purpose::STANDARD.encode(&bytes),
            ),
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
}
