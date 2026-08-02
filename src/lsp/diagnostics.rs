//! Live diagnostics, published on every edit.
//!
//! Two sources, and the split is the milestone's own. Anything `compile`
//! refuses is a *refusal* — an alias that cycles, a dict keyed by a class, an
//! annotated binding with no value — and appears here because the program will
//! not run. Anything [`check`] finds is a *warning*: v0.7 §5 enforces an
//! annotation at run time, so `let x: int = "s"` is a program that compiles and
//! then fails, and the editor saying so first is a courtesy rather than a rule.
//!
//! That is why the second kind is approximate and the first is not. A refusal
//! firing only where inference happened to succeed would be a rule nobody could
//! state; a squiggle doing the same is an editor doing less on a hard case.


use lsp_server::{Connection, Message, Notification};
use lsp_types::{
    notification::{Notification as _, PublishDiagnostics},

    Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams, Url,
};

use quince::error::QuinceError;
use crate::lsp::position::span_to_range;

pub(crate) fn publish_diagnostics(connection: &Connection, uri: Url, source: &str) -> anyhow::Result<()> {
    let mut diagnostics = Vec::new();

    match quince::compile(source) {
        // A program that will not run at all. One diagnostic, because the
        // parser stops at the first thing it cannot read and everything after
        // it would be a guess.
        Err(err) => diagnostics.push(quince_error_to_diagnostic(source, &err)),
        // A program that compiles. What the pass can see about its types is
        // reported as a warning, since the language checks it when it runs.
        Ok(program) => {
            let types = quince::sema::infer::infer(&program);
            diagnostics.extend(
                quince::sema::check::check(&program, &types)
                    .iter()
                    .map(|err| {
                        let mut diagnostic = quince_error_to_diagnostic(source, err);
                        diagnostic.severity = Some(DiagnosticSeverity::WARNING);
                        diagnostic
                    }),
            );
        }
    }

    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };

    let notif = Notification {
        method: PublishDiagnostics::METHOD.to_string(),
        params: serde_json::to_value(params)?,
    };

    connection.sender.send(Message::Notification(notif))?;
    Ok(())
}

pub(crate) fn quince_error_to_diagnostic(source: &str, err: &QuinceError) -> Diagnostic {
    let range = span_to_range(source, err.span);
    let mut message = err.message.clone();
    if let Some(help) = &err.help {
        message.push_str(&format!("\nhelp: {help}"));
    }

    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("quince".to_string()),
        message,
        related_information: None,
        tags: None,
        data: None,
    }
}

