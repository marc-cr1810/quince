use clap::ValueEnum;
use std::io::IsTerminal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    /// Determines whether stdout output should be colorized.
    pub fn use_color_stdout(self) -> bool {
        self.should_color(std::io::stdout().is_terminal())
    }

    /// Determines whether stderr output should be colorized.
    pub fn use_color_stderr(self) -> bool {
        self.should_color(std::io::stderr().is_terminal())
    }

    fn should_color(self, is_terminal: bool) -> bool {
        match self {
            ColorChoice::Never => false,
            ColorChoice::Always => true,
            ColorChoice::Auto => {
                if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
                    return false;
                }
                if std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| v != "0") {
                    return true;
                }
                is_terminal
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    prefix: &'static str,
}

impl Style {
    pub const BOLD: Style = Style { prefix: "\x1b[1m" };
    pub const DIM: Style = Style { prefix: "\x1b[2m" };
    pub const RED: Style = Style { prefix: "\x1b[31m" };
    pub const BOLD_RED: Style = Style {
        prefix: "\x1b[1;31m",
    };
    pub const GREEN: Style = Style { prefix: "\x1b[32m" };
    pub const BOLD_GREEN: Style = Style {
        prefix: "\x1b[1;32m",
    };
    pub const YELLOW: Style = Style { prefix: "\x1b[33m" };
    pub const BOLD_YELLOW: Style = Style {
        prefix: "\x1b[1;33m",
    };
    pub const BLUE: Style = Style { prefix: "\x1b[34m" };
    pub const BOLD_BLUE: Style = Style {
        prefix: "\x1b[1;34m",
    };
    pub const MAGENTA: Style = Style { prefix: "\x1b[35m" };

    pub const BOLD_MAGENTA: Style = Style {
        prefix: "\x1b[1;35m",
    };
    pub const CYAN: Style = Style { prefix: "\x1b[36m" };
    pub const BOLD_CYAN: Style = Style {
        prefix: "\x1b[1;36m",
    };

    pub fn paint(&self, text: impl std::fmt::Display, enabled: bool) -> String {
        if enabled {
            format!("{}{text}\x1b[0m", self.prefix)
        } else {
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_paint_respects_enabled_flag() {
        assert_eq!(
            Style::BOLD_RED.paint("error", true),
            "\x1b[1;31merror\x1b[0m"
        );
        assert_eq!(Style::BOLD_RED.paint("error", false), "error");
    }

    #[test]
    fn color_choice_never_and_always() {
        assert!(!ColorChoice::Never.should_color(true));
        assert!(!ColorChoice::Never.should_color(false));
        assert!(ColorChoice::Always.should_color(true));
        assert!(ColorChoice::Always.should_color(false));
    }
}
