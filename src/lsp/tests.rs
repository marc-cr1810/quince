use super::*;

use lsp_types::{HoverContents, MarkedString, Position};

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
