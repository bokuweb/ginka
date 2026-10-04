//! What the settings page's MCP server form turns its text into.

use ginka_protocol::model::McpTarget;

/// A URL when the text is one, otherwise a command line split the way a
/// shell would — quotes keep an argument with spaces in one piece.
pub fn target(text: &str) -> McpTarget {
    let text = text.trim();
    if text.starts_with("http://") || text.starts_with("https://") {
        return McpTarget::Url {
            url: text.to_string(),
        };
    }
    let mut words = shlex::split(text).unwrap_or_default().into_iter();
    McpTarget::Command {
        program: words.next().unwrap_or_default(),
        args: words.collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_or_a_command_line_with_its_quotes_kept() {
        assert_eq!(
            target(" https://docs.example/mcp "),
            McpTarget::Url {
                url: "https://docs.example/mcp".into()
            }
        );
        assert_eq!(
            target(r#"npx -y "my server" --flag"#),
            McpTarget::Command {
                program: "npx".into(),
                args: vec!["-y".into(), "my server".into(), "--flag".into()],
            }
        );
        assert_eq!(
            target(""),
            McpTarget::Command {
                program: String::new(),
                args: Vec::new()
            },
            "empty is left for the daemon to refuse with its reason"
        );
    }
}
