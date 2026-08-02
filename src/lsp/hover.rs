//! Hover, and signature help.
//!
//! Both answer the same question from different ends: what is this name, and
//! which of its parameters is the cursor sitting in.


use lsp_types::{
    Hover, HoverContents, LanguageString,
    MarkedString, ParameterInformation, ParameterLabel, Position,
    SignatureHelp, SignatureInformation,
};

use quince::sema::symbols::{Kind, Symbol};
use crate::lsp::DocumentState;
use crate::lsp::completion::{find_call_context, rendered_doc, text_before_word};
use crate::lsp::position::{get_word_at_position, position_to_offset};

pub(crate) fn symbol_at(state: &DocumentState, word: &str, pos: Position) -> Option<Symbol> {
    let offset = position_to_offset(&state.text, pos);

    // `p.magnitude` — the word is a member, and the receiver decides which one.
    if let Some(before) = text_before_word(&state.text, pos)
        && let Some(found) = state
            .members_before(&before, offset)
            .into_iter()
            .find(|symbol| symbol.name == word)
    {
        return Some(found);
    }

    if let Some(types) = &state.types
        && let Some(found) = types.symbol(word, offset)
    {
        return Some(found);
    }

    quince::sema::symbols::globals()
        .into_iter()
        .find(|symbol| symbol.name == word)
}

pub(crate) fn get_hover(state: Option<&DocumentState>, pos: Position) -> Option<Hover> {
    let state = state?;
    let word = get_word_at_position(&state.text, pos)?;

    // A keyword explains itself, from beside the list that reserves it.
    if let Some(doc) = quince::syntax::token::TokenKind::keyword(&word).and_then(|kind| kind.doc()) {
        return Some(Hover {
            contents: HoverContents::Array(vec![
                MarkedString::LanguageString(LanguageString {
                    language: "quince".to_string(),
                    value: word,
                }),
                MarkedString::String(doc.to_string()),
            ]),
            range: None,
        });
    }

    let symbol = symbol_at(state, &word, pos)?;
    let signature = symbol.signature();
    // A hover that would repeat the word under the cursor and add nothing says
    // nothing. A parameter with no type and no `@param` is the ordinary case,
    // and an empty tooltip is worse than none at all.
    if symbol.doc.is_none() && signature == word {
        return None;
    }
    let mut contents = vec![MarkedString::LanguageString(LanguageString {
        language: "quince".to_string(),
        value: signature,
    })];
    if let Some(doc) = &symbol.doc {
        contents.push(MarkedString::String(rendered_doc(doc)));
    }

    Some(Hover {
        contents: HoverContents::Array(contents),
        range: None,
    })
}

pub(crate) fn get_signature_help(state: Option<&DocumentState>, pos: Position) -> Option<SignatureHelp> {
    let state = state?;
    let (callee, receiver, active) = find_call_context(&state.text, pos)?;
    let offset = position_to_offset(&state.text, pos);

    // Where to look for the name depends on what is in front of it, exactly as
    // it does for a completion. A method is found on its receiver's class; a
    // bare name is found in scope, and failing that among the globals.
    let symbol = match &receiver {
        Some(before) => state
            .members_before(before, offset)
            .into_iter()
            .find(|symbol| symbol.name == callee),
        None => state
            .types
            .as_ref()
            .and_then(|types| types.symbol(&callee, offset))
            .or_else(|| {
                quince::sema::symbols::globals()
                    .into_iter()
                    .find(|symbol| symbol.name == callee)
            }),
    }?;

    // A name that is not callable has no signature to show. A class is: calling
    // one runs its `init`, and the parameters worth showing are that method's.
    let parameters: Vec<ParameterInformation> = match symbol.kind {
        Kind::Class => match &state.types {
            Some(types) => types
                .members_of(&symbol.name)
                .into_iter()
                .find(|member| member.name == "init")
                .map(|init| init.params)
                .unwrap_or(symbol.params.clone()),
            None => symbol.params.clone(),
        },
        _ => symbol.params.clone(),
    }
    .into_iter()
    .map(|name| ParameterInformation {
        // Documented from the `@param` that named it, when there is one, which
        // is what makes writing them worth the trouble.
        documentation: symbol
            .doc
            .as_ref()
            .and_then(|doc| doc.params.iter().find(|param| param.name == name))
            .map(|param| lsp_types::Documentation::String(param.text.clone())),
        label: ParameterLabel::Simple(name),
    })
    .collect();

    let label = match symbol.kind {
        Kind::Class => format!(
            "{}({})",
            symbol.name,
            parameters
                .iter()
                .map(|param| match &param.label {
                    ParameterLabel::Simple(name) => name.clone(),
                    ParameterLabel::LabelOffsets(_) => String::new(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => symbol.signature(),
    };

    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: symbol
                .doc
                .as_ref()
                .map(|doc| lsp_types::Documentation::String(doc.summary.clone())),
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    })
}
