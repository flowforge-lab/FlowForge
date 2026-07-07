//! Parse `.ipynb` (Jupyter Notebook v4) JSON into a flat list of code cells.

use serde_json::Value;

/// A single code cell extracted from a notebook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotebookCell {
    /// 0-based index of this cell among ALL cells (including non-code) in the
    /// original notebook — so the agent can reference "cell 3" unambiguously.
    pub index: usize,
    /// Joined source code (lines concatenated).
    pub source: String,
}

/// Parse an ipynb JSON string, returning only the code cells. Markdown and raw
/// cells are skipped but still count toward the index.
pub fn parse_notebook(content: &str) -> Result<Vec<NotebookCell>, String> {
    let root: Value = serde_json::from_str(content).map_err(|e| format!("invalid JSON: {e}"))?;

    let cells = root
        .get("cells")
        .and_then(Value::as_array)
        .ok_or("missing or invalid `cells` array")?;

    let mut result = Vec::new();
    for (i, cell) in cells.iter().enumerate() {
        let cell_type = cell.get("cell_type").and_then(Value::as_str).unwrap_or("");
        if cell_type != "code" {
            continue;
        }
        let source = extract_source(cell);
        // Skip empty code cells — nothing to execute.
        if source.trim().is_empty() {
            continue;
        }
        result.push(NotebookCell { index: i, source });
    }
    Ok(result)
}

/// Extract the `source` field which can be either a single string or an array
/// of line strings (both are valid ipynb v4).
fn extract_source(cell: &Value) -> String {
    match cell.get("source") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(lines)) => lines
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}
