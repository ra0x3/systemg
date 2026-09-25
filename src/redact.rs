//! Masks credentials in command lines before they are shown to a caller.

use std::sync::LazyLock;

use regex::Regex;

static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([A-Za-z][A-Za-z0-9+.\-]*://)[^\s/?#'"]*@"#)
        .expect("userinfo pattern compiles")
});

/// Replaces the userinfo of every URL in `command` with `***`, so
/// `http://user:pass@host` reads as `http://***@host`.
///
/// Display only: the username is masked too, since proxy and database
/// usernames are often account identifiers in their own right.
pub fn redact_command(command: &str) -> String {
    URL_USERINFO.replace_all(command, "${1}***@").into_owned()
}

#[cfg(test)]
mod tests {
    use super::redact_command;

    #[test]
    fn masks_user_and_password() {
        assert_eq!(
            redact_command(
                "scraper --proxy http://user-abc:s3cr3t@gate.decodo.com:7000 run"
            ),
            "scraper --proxy http://***@gate.decodo.com:7000 run"
        );
    }

    #[test]
    fn masks_every_url_and_quoted_urls() {
        assert_eq!(
            redact_command("a 'postgres://u:p@db/x' b=redis://:tok@cache:6379"),
            "a 'postgres://***@db/x' b=redis://***@cache:6379"
        );
    }

    #[test]
    fn masks_up_to_the_last_at_sign() {
        assert_eq!(
            redact_command("http://user:p@ss@host/path"),
            "http://***@host/path"
        );
    }

    #[test]
    fn leaves_plain_commands_alone() {
        for command in [
            "python -m http.server 8080",
            "curl https://example.com/a@b",
            "git clone git@github.com:ra0x3/systemg.git",
            "mail user@example.com",
        ] {
            assert_eq!(redact_command(command), command);
        }
    }
}
