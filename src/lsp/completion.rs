//! What to offer after a keystroke.
//!
//! Three cases and they are genuinely different: after a `.`, where the receiver
//! decides; after `from module import`, where the module does; and in the open,
//! where the scope does. v0.7 adds a fourth — after a `:`, where the answer is
//! the type names in scope.


use lsp_types::{

    CompletionItem, CompletionItemKind, Position,
};

use quince::sema::symbols::{Kind, Symbol};
use quince::syntax::doc::Doc;
use quince::syntax::token::KEYWORDS;
use crate::cursor::{ImportSite, import_site};
use crate::lsp::DocumentState;
use crate::lsp::position::position_to_offset;

/// How a symbol is drawn in a completion list.
pub(crate) fn kind_of(kind: Kind) -> CompletionItemKind {
    match kind {
        Kind::Class => CompletionItemKind::CLASS,
        Kind::Function => CompletionItemKind::FUNCTION,
        Kind::Method => CompletionItemKind::METHOD,
        Kind::Field => CompletionItemKind::FIELD,
        Kind::Variable => CompletionItemKind::VARIABLE,
        Kind::Parameter => CompletionItemKind::VARIABLE,
        Kind::Module => CompletionItemKind::MODULE,
    }
}

/// One completion, built from what the pass and the tables know.
///
/// The detail line is the symbol's own signature and the documentation is its
/// own `##` block. Neither is written here, which is the point: this file used
/// to carry sentences about `print` and a guess about everything else.
pub(crate) fn item_of(symbol: &Symbol) -> CompletionItem {
    CompletionItem {
        label: symbol.name.clone(),
        kind: Some(kind_of(symbol.kind)),
        detail: Some(symbol.signature()),
        documentation: symbol.doc.as_ref().map(|doc| {
            lsp_types::Documentation::MarkupContent(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: rendered_doc(doc),
            })
        }),
        ..Default::default()
    }
}

/// A doc block as markdown: the summary, then what it says about each part.
pub(crate) fn rendered_doc(doc: &Doc) -> String {
    let mut out = doc.summary.clone();
    for param in &doc.params {
        out.push_str(&format!("\n\n`{}` — {}", param.name, param.text));
    }
    if let Some(returns) = &doc.returns {
        out.push_str(&format!("\n\nReturns {}", returns.text));
    }
    for thrown in &doc.throws {
        out.push_str(&format!("\n\nRaises `{}` {}", thrown.name, thrown.text));
    }
    out.trim().to_string()
}

pub(crate) fn get_completions(state: Option<&DocumentState>, pos: Position) -> Vec<CompletionItem> {
    let Some(state) = state else {
        return Vec::new();
    };

    // After a dot, the receiver decides the whole list. What the pass does not
    // know, nobody offers: the text heuristics that used to answer here read
    // the first character of the line and told a list it was a string, and an
    // empty list is a better answer than a confident wrong one.
    if is_preceded_by_dot(&state.text, pos) {
        let Some(before) = text_before_dot(&state.text, pos) else {
            return Vec::new();
        };
        let offset = position_to_offset(&state.text, pos);
        return state
            .members_before(&before, offset)
            .iter()
            .map(item_of)
            .collect();
    }

    // An `import` line wants one of two lists and they are not the same list.
    // A module is not there until asked for — that is the whole point of
    // `import`, and offering `math` to a file that never imported it undoes it
    // — while after `from math import` the only things that can follow are the
    // names `math` declares.
    if let Some(line) = state.text.lines().nth(pos.line as usize) {
        let col = (pos.character as usize).min(line.len());
        match import_site(&line[..col]) {
            Some(ImportSite::Module) => {
                return quince::builtins::stdlib::MODULES
                    .iter()
                    .map(|module| CompletionItem {
                        label: module.name.to_string(),
                        kind: Some(CompletionItemKind::MODULE),
                        detail: Some(format!(
                            "{} — {}",
                            module.name,
                            module
                                .members
                                .iter()
                                .map(|(name, _)| *name)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                        ..Default::default()
                    })
                    .collect();
            }
            Some(ImportSite::Member(module)) => {
                return quince::sema::symbols::module_symbols(&module)
                    .iter()
                    .map(item_of)
                    .collect();
            }
            None => {}
        }
    }

    // The keywords, the globals, and everything the pass found in scope here.
    // All three are read off the language rather than written down: a keyword
    // comes with its own explanation, a global with its own signature, and a
    // name in scope with whatever the program said about it.
    let mut items: Vec<CompletionItem> = KEYWORDS
        .iter()
        .map(|keyword| CompletionItem {
            label: (*keyword).to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: quince::syntax::token::TokenKind::keyword(keyword)
                .and_then(|kind| kind.doc())
                .map(str::to_string),
            ..Default::default()
        })
        .collect();
    items.extend(quince::sema::symbols::globals().iter().map(item_of));
    if let Some(types) = &state.types {
        let offset = position_to_offset(&state.text, pos);
        items.extend(types.in_scope(offset).iter().map(item_of));
    }
    items
}

pub(crate) fn is_preceded_by_dot(source: &str, pos: Position) -> bool {
    let line = match source.lines().nth(pos.line as usize) {
        Some(l) => l,
        None => return false,
    };
    let col = (pos.character as usize).min(line.len());
    line[..col].trim_end().ends_with('.')
}

/// The text the cursor sits immediately after the dot of.
pub(crate) fn text_before_dot(source: &str, pos: Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = (pos.character as usize).min(line.len());
    Some(line[..col].trim_end().strip_suffix('.')?.to_string())
}

/// The text a member access sits after, if the word under the cursor is one.
///
/// Hovering the `magnitude` in `p.magnitude()` asks about a method, and which
/// method depends on what `p` is. The word itself is not enough.
pub(crate) fn text_before_word(source: &str, pos: Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = (pos.character as usize).min(line.len());
    let start = line[..col]
        .rfind(|c: char| !(c == '_' || c.is_alphanumeric()))
        .map_or(0, |index| index + 1);
    Some(line[..start].trim_end().strip_suffix('.')?.to_string())
}

/// What is being called at `pos`, and which argument the cursor is in.
///
/// Text, and unavoidably so: the call has not been written yet, so there is no
/// node in any tree to ask. What it reads is bracket depth and commas — where
/// the cursor *is*, never what anything *means*. Deciding the callee's type is
/// the pass's, and this hands the name over for it to answer.
pub(crate) fn find_call_context(source: &str, pos: Position) -> Option<(String, Option<String>, u32)> {
    let before = &source[..(position_to_offset(source, pos) as usize).min(source.len())];

    let chars: Vec<(usize, char)> = before.char_indices().collect();
    let mut depth = 0;
    let mut commas = 0;
    let mut open = None;
    for index in (0..chars.len()).rev() {
        match chars[index].1 {
            ')' => depth += 1,
            '(' if depth == 0 => {
                open = Some(chars[index].0);
                break;
            }
            '(' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            _ => {}
        }
    }

    let before_paren = before[..open?].trim_end();
    let start = before_paren
        .rfind(|c: char| !(c == '_' || c.is_alphanumeric()))
        .map_or(0, |index| index + 1);
    if start >= before_paren.len() {
        return None;
    }
    let callee = before_paren[start..].to_string();
    let receiver = before_paren[..start]
        .trim_end()
        .strip_suffix('.')
        .map(str::to_string);

    Some((callee, receiver, commas))
}
