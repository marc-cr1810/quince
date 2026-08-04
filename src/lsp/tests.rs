use super::*;

use lsp_types::{GotoDefinitionResponse, HoverContents, MarkedString, Position, Range};

use crate::cursor::in_type_position;
use crate::lsp::completion::{get_completions, text_before_dot};
use crate::lsp::hover::{get_hover, get_signature_help};
use crate::lsp::position::{offset_to_position, position_to_offset};

#[test]
fn an_import_line_offers_modules_then_their_members() {
    let offered = |line: &str| -> Vec<String> {
        let state = typed(&["", line]);
        get_completions(Some(&state), end_of(line))
            .into_iter()
            .map(|item| item.label)
            .collect()
    };

    let modules = offered("import ");
    assert!(modules.contains(&"math".to_string()), "{modules:?}");
    assert!(!modules.contains(&"floor".to_string()), "{modules:?}");

    // This used to offer `math, io, time, random` — the four names that
    // cannot follow `import` on a `from` line.
    let members = offered("from math import ");
    assert!(members.contains(&"floor".to_string()), "{members:?}");
    assert!(!members.contains(&"math".to_string()), "{members:?}");
    assert!(!members.contains(&"read".to_string()), "{members:?}");

    assert!(offered("from math import abs, ").contains(&"floor".to_string()));
    assert!(offered("from ma").contains(&"math".to_string()));
}

#[test]
fn an_imported_name_keeps_what_the_module_knew_about_it() {
    let src = "from math import floor\n";
    let state = typed(&[src]);
    // `floor` on the import line, which is where it is bound.
    let pos = Position { line: 0, character: 18 };
    assert_eq!(hovered_at(&state, pos).as_deref(), Some("fn floor(n): int"));

    let code = "from math import floor\nfloor(";
    let state = typed(&[src, code]);
    let help = get_signature_help(Some(&state), end_of(code)).expect("an import helps");
    assert_eq!(help.signatures[0].label, "fn floor(n): int");
}

#[test]
fn a_from_import_does_not_bind_the_module() {
    // `from math import floor` binds `floor` and nothing else. `math.`
    // reaching nothing is the language's answer, not a gap.
    let before = "from math import floor\nmath";
    let code = &format!("{before}.");
    let state = typed(&[before, code]);
    assert!(get_completions(Some(&state), end_of(code)).is_empty());
}

#[test]
fn signature_help_extracts_class_constructor_params() {
    let before = "class Point {\n    op init(x, y) {\n        self.x = x\n        self.y = y\n    }\n}\n";
    let code = &format!("{before}\nlet p = Point(3, ");
    let state = typed(&[before, code]);
    let help = get_signature_help(Some(&state), end_of(code)).expect("a constructor helps");
    assert_eq!(help.signatures[0].label, "Point(x, y)");
    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn signature_help_names_what_a_method_returns() {
    let before = "class Point {\n    op init(x, y) {\n        self.x = x\n        self.y = y\n    }\n    fn scaled(k) {\n        return Point(self.x * k, self.y * k)\n    }\n}\nlet p = Point(3, 4)\n";
    let code = &format!("{before}p.scaled(");
    let state = typed(&[before, code]);
    let help = get_signature_help(Some(&state), end_of(code)).expect("a method helps");
    // The return type comes from the pass, and the parameter name from the
    // declaration. Neither was available when this said `fn scaled(k)`.
    assert_eq!(help.signatures[0].label, "fn scaled(k): Point");
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn signature_help_names_a_builtins_parameters() {
    let before = "let text = \"hello world\"\n";
    let code = &format!("{before}text.split(");
    let state = typed(&[before, code]);
    let help = get_signature_help(Some(&state), end_of(code)).expect("a builtin helps");
    // `fn split(arg1, arg2)` before the natives knew their own parameters.
    assert_eq!(help.signatures[0].label, "fn split(separator): list");
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn signature_help_reaches_a_method_on_a_literal() {
    let before = "\"test\"";
    let code = &format!("{before}.split(");
    let state = typed(&[before, code]);
    let help = get_signature_help(Some(&state), end_of(code)).expect("a literal helps");
    assert_eq!(help.signatures[0].label, "fn split(separator): list");
}

#[test]
fn signature_help_reads_an_error_class_from_the_prelude() {
    // `Error` and its `op init(message)` are written in Quince, so the
    // editor learns the parameter by inferring over the same source the
    // interpreter runs rather than from a second copy of it.
    let code = "TypeError(";
    let state = typed(&["", code]);
    let help = get_signature_help(Some(&state), end_of(code)).expect("an error class helps");
    assert_eq!(help.signatures[0].label, "TypeError(message)");
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn completion_excludes_symbols_defined_after_cursor() {
    let code = "let defined_above = 1\n\nlet defined_below = 2";
    let state = DocumentState::new(code.to_string(), None);
    let pos = Position { line: 1, character: 0 };
    let labels: Vec<String> = get_completions(Some(&state), pos)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert!(labels.contains(&"defined_above".to_string()));
    assert!(!labels.contains(&"defined_below".to_string()));
}

#[test]
fn signature_help_extracts_inherited_constructor_params() {
    let before = "class Animal {\n    op init(name) {\n        self.name = name\n    }\n}\nclass Cat extends Animal {}\n";
    let code = &format!("{before}Cat(");
    let state = typed(&[before, code]);
    let help = get_signature_help(Some(&state), end_of(code)).expect("an inherited init helps");
    assert_eq!(help.signatures[0].label, "Cat(name)");
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn signature_help_extracts_super_method_params() {
    let before = "class Animal {\n    op init(name) {\n        self.name = name\n    }\n}\nclass Dog extends Animal {\n    op init(name, breed) {\n        self.breed = breed\n    }\n}\n";
    let code = "class Animal {\n    op init(name) {\n        self.name = name\n    }\n}\nclass Dog extends Animal {\n    op init(name, breed) {\n        super.init(";
    let state = typed(&[before, code]);
    let help = get_signature_help(Some(&state), end_of(code)).expect("`super.init` helps");
    assert_eq!(help.signatures[0].label, "fn init(name)");
    assert_eq!(help.active_parameter, Some(0));
}

/// The position just past the end of `code`, which is where the cursor is
/// when someone has just typed the last character of it.
fn end_of(code: &str) -> Position {
    let line = code.matches('\n').count() as u32;
    let start = code.rfind('\n').map_or(0, |index| index + 1);
    Position {
        line,
        character: code[start..].chars().count() as u32,
    }
}

fn path_at(code: &str) -> Option<String> {
    path_ending_at(&text_before_dot(code, end_of(code))?)
}

/// A document typed rather than opened: each string is the whole text after
/// one edit.
///
/// Which is the only honest way to test a completion after a `.`, because
/// the `.` is what stops the document parsing — a state built from the
/// final text alone would be testing a document nobody ever has.
fn typed(edits: &[&str]) -> DocumentState {
    let mut state = None;
    for text in edits {
        state = Some(DocumentState::new(text.to_string(), state));
    }
    state.expect("at least one edit")
}

/// The labels a completion request comes back with, after typing the `.`.
fn completions_after(before: &str) -> Vec<String> {
    let code = format!("{before}.");
    let state = typed(&[before, &code]);
    let mut labels: Vec<String> = get_completions(Some(&state), end_of(&code))
        .into_iter()
        .map(|item| item.label)
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

#[test]
fn a_receiver_path_keeps_the_parentheses_the_heuristics_throw_away() {
    assert_eq!(path_at("p.").as_deref(), Some("p"));
    assert_eq!(path_at("o.inner.").as_deref(), Some("o.inner"));
    assert_eq!(path_at("Point().").as_deref(), Some("Point()"));
    // The arguments say nothing about the type, so they are normalised
    // away rather than parsed.
    assert_eq!(path_at("Point(3, 4).").as_deref(), Some("Point()"));
    assert_eq!(path_at("f(g(x)).").as_deref(), Some("f()"));
    assert_eq!(path_at("let q = b.twin().").as_deref(), Some("b.twin()"));

    // Not a path, and not approximated into one — the heuristics read
    // literals, and this is not a parser.
    assert_eq!(path_at("\"abc\"."), None);
    assert_eq!(path_at("xs[0]."), None);
    assert_eq!(path_at("p"), None);
}

#[test]
fn a_position_and_an_offset_name_the_same_byte() {
    let code = "let a = 1\nfn f() {\n  return 2\n}\n";
    for offset in 0..=code.len() {
        let position = offset_to_position(code, offset);
        assert_eq!(
            position_to_offset(code, position) as usize,
            offset,
            "offset {offset} did not survive the round trip"
        );
    }
}

#[test]
fn dot_completion_offers_the_class_the_pass_worked_out() {
    // The receiver is a constructor call, which the capital-letter
    // heuristic gets right by accident and the pass gets right by reading
    // the declaration. What the heuristic cannot do is the second half:
    // `made` holds a `Box` because that is what `make` returns.
    let labels = completions_after("class Box {\n    op init() {\n        self.n = 1\n    }\n    fn only_on_box() {\n        return 1\n    }\n}\nfn make() {\n    return Box()\n}\nlet made = make()\nmade");
    assert!(labels.contains(&"only_on_box".to_string()), "{labels:?}");
    assert!(labels.contains(&"n".to_string()), "{labels:?}");
}

const HAS_HIDDEN_MEMBERS: &str = "class Account {\n\
     private let balance = 0\n\
     protected let owner = \"nobody\"\n\
     public final id = \"A1\"\n\
     fn peek() {\n\
     return 1\n\
     }\n\
     private fn audit() {\n\
     return 2\n\
     }\n\
     }\n";

#[test]
fn dot_completion_withholds_what_the_language_would_refuse() {
    // Outside the class, only the public members are worth offering — a list
    // that suggests `balance` is a list that suggests writing a VisibilityError.
    let labels = completions_after(&format!("{HAS_HIDDEN_MEMBERS}let a = Account()\na"));
    assert!(labels.contains(&"peek".to_string()), "{labels:?}");
    assert!(labels.contains(&"id".to_string()), "{labels:?}");
    assert!(!labels.contains(&"balance".to_string()), "{labels:?}");
    assert!(!labels.contains(&"owner".to_string()), "{labels:?}");
    assert!(!labels.contains(&"audit".to_string()), "{labels:?}");
}

#[test]
fn dot_completion_offers_a_modules_members() {
    // Nothing offered these before: a module is not a class, so the
    // class-shaped heuristic had nothing to say about `math.` at all.
    let labels = completions_after("import math\nmath");
    assert!(labels.contains(&"floor".to_string()), "{labels:?}");
    assert!(labels.contains(&"pi".to_string()), "{labels:?}");
    // And only that module's, which is the point of `import` — `read` is
    // `io`'s and has no business here.
    assert!(!labels.contains(&"read".to_string()), "{labels:?}");
}

/// The code block a hover leads with, which is the signature.
fn hovered(code: &str, pos: Position) -> Option<String> {
    hovered_at(&DocumentState::new(code.to_string(), None), pos)
}

fn hovered_at(state: &DocumentState, pos: Position) -> Option<String> {
    let hover = get_hover(Some(state), pos)?;
    let HoverContents::Array(parts) = hover.contents else {
        panic!("expected an array of hover contents");
    };
    match parts.first() {
        Some(MarkedString::LanguageString(shown)) => Some(shown.value.clone()),
        _ => panic!("expected a quince code block first"),
    }
}

/// Everything a hover says after its signature.
fn hovered_doc(code: &str, pos: Position) -> Option<String> {
    let state = DocumentState::new(code.to_string(), None);
    let HoverContents::Array(parts) = get_hover(Some(&state), pos)?.contents else {
        panic!("expected an array of hover contents");
    };
    match parts.get(1) {
        Some(MarkedString::String(text)) => Some(text.clone()),
        _ => None,
    }
}

#[test]
fn hover_over_a_binding_says_how_it_was_bound_and_what_it_holds() {
    let pos = Position { line: 0, character: 5 };
    assert_eq!(
        hovered("let total = 1 + 2\n", pos).as_deref(),
        Some("let total: int")
    );
    // `final` and `const` are a real distinction and the editor shows it.
    assert_eq!(
        hovered("const LIMIT = 10\n", Position { line: 0, character: 7 }).as_deref(),
        Some("const LIMIT: int")
    );
}

#[test]
fn hover_over_a_function_names_what_it_returns() {
    let code = "fn make() {\n    return 1\n}\n";
    let pos = Position { line: 0, character: 4 };
    assert_eq!(hovered(code, pos).as_deref(), Some("fn make(): int"));
}

#[test]
fn hover_over_a_keyword_explains_it_from_beside_the_list() {
    let code = "let total = 1\n";
    let doc = hovered_doc(code, Position { line: 0, character: 1 })
        .expect("a keyword explains itself");
    assert!(doc.contains("reassigned"), "{doc}");
}

#[test]
fn hover_over_a_builtin_reads_the_natives_own_documentation() {
    // This used to be a sentence written in the language server for ten builtins and
    // nothing for the other forty-two.
    let code = "let n = len(\"ab\")\n";
    let pos = Position { line: 0, character: 9 };
    assert_eq!(hovered(code, pos).as_deref(), Some("fn len(value): int"));
    let doc = hovered_doc(code, pos).expect("`len` documents itself");
    assert!(doc.contains("How many characters"), "{doc}");
}

#[test]
fn hover_over_a_parameter_says_what_its_function_said_about_it() {
    // The only thing anyone can be told about a parameter until v0.7, and
    // the reason writing an `@param` is worth the trouble.
    let code = "## Scales it.\n## @param k how much to scale by\nfn scale(k) {\n    return k\n}\n";
    let pos = Position { line: 3, character: 11 };
    assert_eq!(
        hovered_doc(code, pos).as_deref(),
        Some("how much to scale by")
    );
}

#[test]
fn hover_says_nothing_where_nothing_is_known() {
    // An undocumented parameter carries no type and no prose. Repeating the
    // word under the cursor back at the reader is worse than silence.
    let code = "fn f(x) {\n    return x\n}\n";
    assert!(hovered(code, Position { line: 1, character: 11 }).is_none());
}

/// A native's result is what the native says it is.
///
/// This asserted the opposite until `Native` grew a `returns` field: the
/// pass had nothing to read, the heuristics answered, and they read the
/// literal at the front of the line and called a list a string. `words` is
/// a list, and the editor now offers a list's methods on it.
#[test]
fn dot_completion_types_a_literal_by_lexing_it() {
    // `[1, 2]` is a list and `xs[0]` is an item out of one. The heuristic
    // that used to answer here read the trailing bracket and called both
    // lists; this reads the token in front of the opening one.
    let labels = completions_after("[1, 2]");
    assert!(labels.contains(&"push".to_string()), "{labels:?}");

    let labels = completions_after("let xs = [1, 2]\nxs[0]");
    assert!(labels.is_empty(), "an item of unknown type offers nothing: {labels:?}");

    let labels = completions_after("\"abc\"");
    assert!(labels.contains(&"upper".to_string()), "{labels:?}");
}

#[test]
fn dot_completion_on_an_unknown_receiver_offers_nothing() {
    // The heuristics used to fill this in by guessing. An empty list is the
    // honest answer, and a wrong one is worse than none: it is indexed,
    // scrolled, and believed.
    let labels = completions_after("fn f(thing) {\n    thing");
    assert!(labels.is_empty(), "{labels:?}");
}

#[test]
fn a_natives_result_is_what_the_native_says_it_is() {
    let labels = completions_after("let words = \"a,b,c\".split(\",\")\nwords");
    assert!(labels.contains(&"push".to_string()), "{labels:?}");
    assert!(!labels.contains(&"lower".to_string()), "{labels:?}");

    // And the other direction. `join` is a string's method taking the list,
    // so this crosses from a list back to a string — which the heuristics
    // got wrong the same way, by reading the `[` and stopping.
    let labels = completions_after("let sep = \", \"\nlet line = sep.join([\"a\", \"b\"])\nline");
    assert!(labels.contains(&"upper".to_string()), "{labels:?}");
    assert!(!labels.contains(&"push".to_string()), "{labels:?}");
}

#[test]
fn a_colon_in_a_declaration_offers_types() {
    // A document that parsed, and then the `:` that stops it parsing — which is
    // the only state anyone is ever in when they want this list.
    let valid = "class Point {\n    op init() { }\n}\nalias Score = int\n";
    let before = &format!("{valid}let x:");
    let state = typed(&[valid, before]);
    let labels: Vec<String> = get_completions(Some(&state), end_of(before))
        .into_iter()
        .map(|item| item.label)
        .collect();
    // The builtins, the keyword that is not one, and what the program declared.
    assert!(labels.contains(&"int".to_string()), "{labels:?}");
    assert!(labels.contains(&"string".to_string()), "{labels:?}");
    assert!(labels.contains(&"any".to_string()), "{labels:?}");
    assert!(labels.contains(&"Point".to_string()), "{labels:?}");
    // A value in scope is not a type, so it is not offered here.
    assert!(!labels.contains(&"print".to_string()), "{labels:?}");
}

#[test]
fn a_colon_that_is_not_an_annotation_offers_no_types() {
    // The grammar's other two colons. A dict literal's follows a `{` or a `,`,
    // and a slice's follows a `[` — both on the same line, which is what tells
    // them apart from an annotation without a tree to ask.
    for line in ["let d = {\"a\":", "let s = xs[1:", "let d = {\"a\": 1, \"b\":"] {
        assert!(
            !in_type_position(line, line.len()),
            "{line:?} is not type position"
        );
    }
    for line in ["let x:", "let x: ", "fn f(n:", "fn f(): ", "let xs: list[i"] {
        assert!(
            in_type_position(line, line.len()),
            "{line:?} is type position"
        );
    }
}

#[test]
fn a_hint_is_offered_only_where_the_program_said_nothing() {
    let src = "let n = 8\nlet named: string = \"a\"\nlet unknown = nothing()\n";
    let state = DocumentState::new(src.to_string(), None);
    let whole = Range {
        start: Position { line: 0, character: 0 },
        end: Position { line: 9, character: 0 },
    };
    let hints = crate::lsp::hints::get_inlay_hints(Some(&state), whole);
    let labels: Vec<String> = hints
        .iter()
        .map(|hint| match &hint.label {
            lsp_types::InlayHintLabel::String(text) => text.clone(),
            other => panic!("expected a plain label, got {other:?}"),
        })
        .collect();
    // `n` is inferred and unannotated, so it gets one. `named` said it already.
    // `unknown` is `Unknown`, and a margin full of `: _` is an editor saying
    // nothing loudly.
    assert_eq!(labels, vec![": int".to_string()]);

    // And it lands just past the name, where the annotation would have gone.
    assert_eq!(hints[0].position, Position { line: 0, character: 5 });
}

#[test]
fn a_certain_mistake_is_drawn_as_an_error() {
    // The check reports only where the pass knows both types and they
    // definitely disagree, so a report is a certainty — the line will fail when
    // it runs. Drawing that in the colour reserved for suspicions would teach a
    // reader to skim past the ones that are certain.
    let src = "let x: int = \"s\"\n";
    let state = DocumentState::new(src.to_string(), None);
    let program = quince::compile(state.text()).expect("it compiles — that is the point");
    let types = quince::sema::infer::infer(&program);
    let found = quince::sema::check::check(&program, &types);
    assert_eq!(found.len(), 1, "{found:?}");

    let diagnostic = crate::lsp::diagnostics::quince_error_to_diagnostic(src, &found[0]);
    assert_eq!(
        diagnostic.severity,
        Some(lsp_types::DiagnosticSeverity::ERROR)
    );
}

#[test]
fn test_references_and_rename_handlers() {
    let src = "fn add(a, b) {\n    return a + b\n}\nlet total = add(1, 2)\n";
    let state = DocumentState::new(src.to_string(), None);
    let uri = "file:///test.qn".parse().unwrap();
    let pos = Position { line: 0, character: 3 }; // 'add'
    let empty_docs = HashMap::new();

    let refs = crate::lsp::navigate::get_references(&uri, Some(&state), &empty_docs, pos);
    assert_eq!(refs.len(), 2);

    let edit = crate::lsp::navigate::rename_symbol(&uri, Some(&state), &empty_docs, pos, "sum").unwrap();
    let changes = edit.changes.unwrap();
    assert_eq!(changes.get(&uri).unwrap().len(), 2);
}

#[test]
fn test_workspace_symbols_search() {
    let src = "class Calculator {\n    fn calculate() {\n        return 0\n    }\n}\n";
    let state = DocumentState::new(src.to_string(), None);
    let uri = "file:///calc.qn".parse().unwrap();
    let mut docs = HashMap::new();
    docs.insert(uri, state);

    let syms = crate::lsp::navigate::get_workspace_symbols(&docs, "calc");
    assert_eq!(syms.len(), 2); // Calculator and calculate
}

#[test]
fn test_document_formatting() {
    let unformatted = "fn test() {\nlet x = 1\n}\n";
    let state = DocumentState::new(unformatted.to_string(), None);
    let edits = crate::lsp::format::format_document(
        Some(&state),
        lsp_types::DocumentFormattingParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: "file:///test.qn".parse().unwrap(),
            },
            options: lsp_types::FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        },
    );
    assert!(!edits.is_empty());
    assert_eq!(edits[0].new_text, "fn test() {\n    let x = 1\n}\n");
}

#[test]
fn test_hierarchical_document_symbols() {
    let src = "class Point {\n    let x = 0\n    fn move() {\n        return 1\n    }\n}\n";
    let state = DocumentState::new(src.to_string(), None);
    let uri = "file:///point.qn".parse().unwrap();
    let symbols = crate::lsp::navigate::get_hierarchical_document_symbols(&uri, Some(&state));
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "Point");
    let children = symbols[0].children.as_ref().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].name, "x");
    assert_eq!(children[0].kind, lsp_types::SymbolKind::FIELD);
    assert_eq!(children[1].name, "move");
    assert_eq!(children[1].kind, lsp_types::SymbolKind::METHOD);
}

#[test]
fn test_ast_aware_references_ignores_comments_and_strings() {
    let src = "// add is cool\nlet add = 1\nlet msg = \"add\"\nlet total = add + 2\n";
    let state = DocumentState::new(src.to_string(), None);
    let uri = "file:///test.qn".parse().unwrap();
    let pos = Position { line: 1, character: 4 }; // 'add' variable
    let empty_docs = HashMap::new();
    let refs = crate::lsp::navigate::get_references(&uri, Some(&state), &empty_docs, pos);
    assert_eq!(refs.len(), 2);
}

#[test]
fn test_expanded_code_actions() {
    let src = "let n = math.floor(1.5)\n";
    let state = DocumentState::new(src.to_string(), None);
    let uri = "file:///test.qn".parse().unwrap();

    let diag = lsp_types::Diagnostic {
        range: Range {
            start: Position { line: 0, character: 8 },
            end: Position { line: 0, character: 12 },
        },
        severity: Some(lsp_types::DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("quince".to_string()),
        message: "unknown module math".to_string(),
        related_information: None,
        tags: None,
        data: None,
    };

    let params = lsp_types::CodeActionParams {
        text_document: lsp_types::TextDocumentIdentifier { uri },
        range: Range {
            start: Position { line: 0, character: 8 },
            end: Position { line: 0, character: 12 },
        },
        context: lsp_types::CodeActionContext {
            diagnostics: vec![diag],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = crate::lsp::actions::get_code_actions(Some(&state), params);
    assert!(!actions.is_empty());
}

#[test]
fn test_folding_ranges() {
    let src = "## Header comment\n## Line 2\nfn foo() {\n    let x = 1\n}\n";
    let state = DocumentState::new(src.to_string(), None);
    let ranges = crate::lsp::folding::get_folding_ranges(Some(&state));
    assert_eq!(ranges.len(), 2);
}

#[test]
fn test_selection_ranges() {
    let src = "fn bar() {\n    let count = 42\n}\n";
    let state = DocumentState::new(src.to_string(), None);
    let pos = Position { line: 1, character: 10 };
    let sel = crate::lsp::selection::get_selection_ranges(Some(&state), vec![pos]);
    assert_eq!(sel.len(), 1);
}

#[test]
fn test_document_highlights() {
    let src = "let total = 10\nlet copy = total + 1\n";
    let state = DocumentState::new(src.to_string(), None);
    let uri = "file:///test.qn".parse().unwrap();
    let pos = Position { line: 0, character: 5 };
    let hl = crate::lsp::highlight::get_document_highlights(&uri, Some(&state), pos);
    assert_eq!(hl.len(), 2);
}

#[test]
fn test_code_lenses() {
    let src = "fn greet() {\n    return \"hi\"\n}\nlet g = greet()\n";
    let state = DocumentState::new(src.to_string(), None);
    let uri = "file:///test.qn".parse().unwrap();
    let empty_docs = HashMap::new();
    let lenses = crate::lsp::codelens::get_code_lenses(&uri, Some(&state), &empty_docs);
    assert_eq!(lenses.len(), 1);
    assert_eq!(lenses[0].command.as_ref().unwrap().title, "1 reference");
}

#[test]
fn test_lsp_cross_document_resolution() {
    let math_utils_uri: Url = "file:///workspace/math_utils.qn".parse().unwrap();
    let math_utils_src = "class Vector {\n op init(x, y) { self.x = x\n self.y = y }\n}\nfn square(x) { return x * x }\n";
    let math_utils_state = DocumentState::new(math_utils_src.to_string(), None);

    let mut documents = HashMap::new();
    documents.insert(math_utils_uri.clone(), math_utils_state);

    let main_uri: Url = "file:///workspace/main.qn".parse().unwrap();
    let main_src = "from math_utils import Vector, square\nlet v = Vector(3, 4)\nlet sq = square(5)\n";
    let main_state = DocumentState::new_with_documents(
        main_src.to_string(),
        Some(&main_uri),
        &documents,
        None,
    );

    let types = main_state.types().expect("types inferred for main");
    let end = main_src.len() as u32;

    assert_eq!(types.of_name("v", end).class_name(), Some("Vector"));

    // Check completion for `from math_utils import `
    let pos = Position { line: 0, character: 23 };
    let completions = get_completions(Some(&main_state), pos);
    let labels: Vec<_> = completions.into_iter().map(|c| c.label).collect();
    assert!(labels.contains(&"Vector".to_string()), "Completions: {labels:?}");
    assert!(labels.contains(&"square".to_string()), "Completions: {labels:?}");

    // Check dot completion on `v.`
    let pos_dot = Position { line: 1, character: 7 }; // right after `v`
    let v_members = main_state.members_before("v", position_to_offset(main_src, pos_dot));
    let member_names: Vec<_> = v_members.into_iter().map(|s| s.name).collect();
    assert!(member_names.contains(&"x".to_string()), "Members: {member_names:?}");
    assert!(member_names.contains(&"y".to_string()), "Members: {member_names:?}");
}

#[test]
fn test_cross_document_diagnostics_no_unknown_type() {
    let matrix_uri: Url = "file:///workspace/matrix.qn".parse().unwrap();
    let matrix_src = "public class Matrix {\n op init() {}\n}\nfn matrix_identity(n) { return Matrix() }\n";
    let matrix_state = DocumentState::new(matrix_src.to_string(), None);

    let mut documents = HashMap::new();
    documents.insert(matrix_uri.clone(), matrix_state);

    let gate_uri: Url = "file:///workspace/gate.qn".parse().unwrap();
    let gate_src = "from matrix import Matrix, matrix_identity\npublic class QuantumGate {\n public fn get_matrix(): Matrix {\n return matrix_identity(2)\n }\n}\n";
    let gate_state = DocumentState::new_with_documents(
        gate_src.to_string(),
        Some(&gate_uri),
        &documents,
        None,
    );

    let (program, errors) = quince::compile_recovering(gate_src);
    assert!(errors.is_empty());
    let types = gate_state.types().expect("types inferred for gate");
    let check_errors = quince::sema::check::check(&program, types);
    assert!(
        check_errors.is_empty(),
        "Expected zero diagnostics/type errors for imported Matrix class, but got: {check_errors:?}"
    );
}

#[test]
fn test_goto_definition_cross_document() {
    let matrix_uri: Url = "file:///workspace/matrix.qn".parse().unwrap();
    let matrix_src = "public class Matrix {\n op init() {}\n}\nfn matrix_identity(n) { return Matrix() }\n";
    let matrix_state = DocumentState::new(matrix_src.to_string(), None);

    let mut documents = HashMap::new();
    documents.insert(matrix_uri.clone(), matrix_state);

    let gate_uri: Url = "file:///workspace/gate.qn".parse().unwrap();
    let gate_src = "from matrix import Matrix, matrix_identity\npublic class QuantumGate {\n public fn get_matrix(): Matrix {\n return matrix_identity(2)\n }\n}\n";
    let gate_state = DocumentState::new_with_documents(
        gate_src.to_string(),
        Some(&gate_uri),
        &documents,
        None,
    );

    // 1. Definition on `Matrix` type annotation in get_matrix(): Matrix
    // Line 2, character 25 is on `Matrix`
    let pos_matrix = Position { line: 2, character: 25 };
    let def_matrix = get_definition(&gate_uri, Some(&gate_state), &documents, pos_matrix);
    assert!(def_matrix.is_some(), "Definition for Matrix should be found");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_matrix {
        assert_eq!(loc.uri, matrix_uri);
    } else {
        panic!("Expected scalar location for Matrix");
    }

    // 2. Definition on `matrix_identity` call
    // Line 3, character 15 is on `matrix_identity`
    let pos_fn = Position { line: 3, character: 15 };
    let def_fn = get_definition(&gate_uri, Some(&gate_state), &documents, pos_fn);
    assert!(def_fn.is_some(), "Definition for matrix_identity should be found");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_fn {
        assert_eq!(loc.uri, matrix_uri);
    } else {
        panic!("Expected scalar location for matrix_identity");
    }

    // 3. Definition on `matrix` module name in `from matrix import ...`
    // Line 0, character 7 is on `matrix`
    let pos_mod = Position { line: 0, character: 7 };
    let def_mod = get_definition(&gate_uri, Some(&gate_state), &documents, pos_mod);
    assert!(def_mod.is_some(), "Definition for matrix module should be found");
    if let Some(GotoDefinitionResponse::Scalar(loc)) = def_mod {
        assert_eq!(loc.uri, matrix_uri);
    } else {
        panic!("Expected scalar location for matrix module");
    }
}

#[test]
fn test_cross_module_reference_count() {
    let complex_uri: Url = "file:///workspace/complex.qn".parse().unwrap();
    let complex_src = "public class Complex {\n op init(re, im) {}\n}\nfn complex_one(): Complex {\n return Complex(1.0, 0.0)\n}\n";
    let complex_state = DocumentState::new(complex_src.to_string(), None);

    let mut documents = HashMap::new();
    documents.insert(complex_uri.clone(), complex_state);

    let main_uri: Url = "file:///workspace/main.qn".parse().unwrap();
    let main_src = "from complex import Complex, complex_one\nlet c1 = complex_one()\nlet c2 = complex_one()\n";
    let main_state = DocumentState::new_with_documents(
        main_src.to_string(),
        Some(&main_uri),
        &documents,
        None,
    );
    documents.insert(main_uri.clone(), main_state);

    let comp_state = documents.get(&complex_uri);
    let lenses = crate::lsp::codelens::get_code_lenses(&complex_uri, comp_state, &documents);
    let fn_lens = lenses
        .iter()
        .find(|l| l.range.start.line == 3)
        .expect("code lens for complex_one");
    assert_eq!(
        fn_lens.command.as_ref().unwrap().title,
        "3 references",
        "Expected 3 references to complex_one across documents"
    );
}

#[test]
fn test_throw_custom_exception_subclass_across_modules() {
    let errors_uri: Url = "file:///workspace/errors.qn".parse().unwrap();
    let errors_src = r#"
public class QuantumError extends Error {
    public final details: string = ""
    op init(message: string, details: string) {
        super.init(message)
        self.details = details
    }
}

public final class CircuitValidationError extends QuantumError {
    op init(reason: string) {
        super.init("Circuit validation failed: " + reason, "")
    }
}
"#;
    let errors_state = DocumentState::new(errors_src.to_string(), None);
    let mut documents = HashMap::new();
    documents.insert(errors_uri.clone(), errors_state);

    let circuit_uri: Url = "file:///workspace/circuit.qn".parse().unwrap();
    let circuit_src = r#"
from errors import CircuitValidationError

fn validate(num_qubits: int) {
    if num_qubits <= 0 {
        throw CircuitValidationError("Circuit must have at least 1 qubit")
    }
}
"#;
    let circuit_state = DocumentState::new_with_documents(
        circuit_src.to_string(),
        Some(&circuit_uri),
        &documents,
        None,
    );

    let (program, errors) = quince::compile_recovering(circuit_src);
    assert!(errors.is_empty());
    let types = circuit_state.types().expect("types inferred for circuit");
    let check_errors = quince::sema::check::check(&program, types);
    assert!(
        check_errors.is_empty(),
        "expected zero diagnostics when throwing CircuitValidationError, got: {:?}",
        check_errors
    );
}

#[test]
fn test_private_method_accessibility_inside_class_with_imports() {
    let errors_uri: Url = "file:///workspace/errors.qn".parse().unwrap();
    let errors_src = r#"
public class CircuitValidationError extends Error {
    op init(msg: string) { super.init(msg) }
}
"#;
    let errors_state = DocumentState::new(errors_src.to_string(), None);
    let mut documents = HashMap::new();
    documents.insert(errors_uri.clone(), errors_state);

    let circuit_uri: Url = "file:///workspace/circuit.qn".parse().unwrap();
    let circuit_src = r#"
from errors import CircuitValidationError

public class QuantumCircuit {
    public final num_qubits: int = 0

    op init(num_qubits: int) {
        self.num_qubits = num_qubits
    }

    private fn validate_qubit(q: int) {
        if q < 0 || q >= self.num_qubits {
            throw CircuitValidationError("out of bounds")
        }
    }

    public fn h(qubit: int) {
        self.validate_qubit(qubit)
        return self
    }
}
"#;
    let circuit_state = DocumentState::new_with_documents(
        circuit_src.to_string(),
        Some(&circuit_uri),
        &documents,
        None,
    );

    let (program, errors) = quince::compile_recovering(circuit_src);
    assert!(errors.is_empty());
    let types = circuit_state.types().expect("types inferred for circuit");
    let check_errors = quince::sema::check::check(&program, types);
    assert!(
        check_errors.is_empty(),
        "expected zero diagnostics for private method call inside class, got: {:?}",
        check_errors
    );
}



