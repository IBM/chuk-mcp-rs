//! Working out what the caller meant by a connection string.

use crate::protocol::types::errors::McpError;

/// URL schemes that mean "talk HTTP to this".
const HTTP_SCHEMES: [&str; 2] = ["http://", "https://"];

/// Where to connect, and therefore which transport to use.
///
/// Deliberately only *where*: how to connect — credentials, timeouts, the
/// environment a subprocess gets — belongs to [`Connect`](super::Connect), so
/// that one target can be reused with different options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A server to spawn as a subprocess and talk to over its stdio.
    Stdio { command: String, args: Vec<String> },
    /// A Streamable HTTP endpoint.
    Http { url: String },
}

impl Target {
    /// Interpret a connection string.
    ///
    /// Anything beginning `http://` or `https://` is an endpoint; anything
    /// else is a command line, split on whitespace. That covers the two things
    /// people actually type, and [`Target::command`] is there for the cases
    /// where whitespace splitting is not good enough — an argument containing
    /// a space, say.
    pub fn parse(target: &str) -> Result<Target, McpError> {
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return Err(McpError::validation("connection target is empty"));
        }

        if HTTP_SCHEMES
            .iter()
            .any(|scheme| trimmed.starts_with(scheme))
        {
            return Ok(Target::Http {
                url: trimmed.to_string(),
            });
        }

        let mut words = trimmed.split_whitespace().map(str::to_string);
        let command = words
            .next()
            .ok_or_else(|| McpError::validation("connection target is empty"))?;
        Ok(Target::Stdio {
            command,
            args: words.collect(),
        })
    }

    /// A subprocess target with the command and arguments given explicitly,
    /// for command lines that whitespace splitting would mangle.
    pub fn command<I, S>(command: impl Into<String>, args: I) -> Target
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Target::Stdio {
            command: command.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// An HTTP target.
    pub fn url(url: impl Into<String>) -> Target {
        Target::Http { url: url.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_become_http_targets() {
        for url in ["http://localhost:3000/mcp", "https://example.com/mcp"] {
            assert_eq!(Target::parse(url).unwrap(), Target::url(url));
        }
    }

    #[test]
    fn a_command_line_splits_into_command_and_args() {
        assert_eq!(
            Target::parse("python -u server.py").unwrap(),
            Target::command("python", ["-u", "server.py"])
        );
    }

    #[test]
    fn a_bare_command_has_no_args() {
        assert_eq!(
            Target::parse("./my-server").unwrap(),
            Target::command("./my-server", Vec::<String>::new())
        );
    }

    #[test]
    fn surrounding_whitespace_is_not_an_argument() {
        assert_eq!(
            Target::parse("  python   server.py  ").unwrap(),
            Target::command("python", ["server.py"])
        );
    }

    #[test]
    fn an_empty_target_is_rejected_rather_than_guessed() {
        assert!(Target::parse("").is_err());
        assert!(Target::parse("   ").is_err());
    }

    #[test]
    fn a_url_is_never_read_as_a_command() {
        // The scheme decides, so a URL with spaces around it is still a URL.
        assert!(matches!(
            Target::parse(" https://example.com/mcp ").unwrap(),
            Target::Http { .. }
        ));
    }
}
