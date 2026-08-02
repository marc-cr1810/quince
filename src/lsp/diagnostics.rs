//! Live diagnostics, published on every edit.


use lsp_server::{Connection, Message, Notification};
use lsp_types::{
    notification::{Notification as _, PublishDiagnostics},

    Diagnostic, DiagnosticSeverity, PublishDiagnosticsParams, Url,
};

use quince::error::QuinceError;
use crate::lsp::position::span_to_range;

pub(crate) fn publish_diagnostics(connection: &Connection, uri: Url, source: &str) -> anyhow::Result<()> {
    let mut diagnostics = Vec::new();

    if let Err(err) = quince::compile(source) {
        diagnostics.push(quince_error_to_diagnostic(source, &err));
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

