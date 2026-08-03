//! Live diagnostics, published on every edit.
//!
//! Two sources, and both are errors. Anything `compile` refuses stops the
//! program from running at all — an alias that cycles, a dict keyed by a class,
//! an annotated binding with no value. Anything `sema::check` finds runs right
//! up until the offending line and then fails there, with the same sentence the
//! editor showed.
//!
//! Neither is a warning, and the second was one for a while. The check is
//! one-sided by construction: it reports only where the pass knows both types
//! and they definitely disagree. That makes a report a certainty rather than a
//! suspicion, and drawing a certainty in the colour reserved for suspicions
//! teaches a reader to skim past it.
//!
//! What separates them is not confidence but *reach*: the first kind means
//! nothing runs, the second means everything up to that line does. The protocol
//! has no severity for that distinction, and inventing one by demoting the
//! second to a warning was the wrong way to express it.


use lsp_server::{Connection, Message, Notification};
use lsp_types::{
    notification::{Notification as _, PublishDiagnostics},

    Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams, Uri as Url,
};

use quince::error::QuinceError;
use crate::lsp::position::span_to_range;

pub(crate) fn publish_diagnostics(connection: &Connection, uri: Url, source: &str) -> anyhow::Result<()> {
    let mut diagnostics = Vec::new();

    let (program, errors) = quince::compile_recovering(source);
    for err in &errors {
        diagnostics.push(quince_error_to_diagnostic(source, err));
    }

    if !program.is_empty() {
        let types = quince::sema::infer::infer(&program);
        diagnostics.extend(
            quince::sema::check::check(&program, &types)
                .iter()
                .map(|err| quince_error_to_diagnostic(source, err)),
        );
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

