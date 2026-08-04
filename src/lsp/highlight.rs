//! Document highlight provider.

use lsp_types::{DocumentHighlight, DocumentHighlightKind, Position, Uri as Url};
use crate::lsp::DocumentState;
use crate::lsp::navigate::get_references;

pub(crate) fn get_document_highlights(
    uri: &Url,
    state: Option<&DocumentState>,
    pos: Position,
) -> Vec<DocumentHighlight> {
    let empty_docs = std::collections::HashMap::new();
    let refs = get_references(uri, state, &empty_docs, pos);
    refs.into_iter()
        .map(|loc| DocumentHighlight {
            range: loc.range,
            kind: Some(DocumentHighlightKind::TEXT),
        })
        .collect()
}
