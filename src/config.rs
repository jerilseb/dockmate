//! The optional config file.
//!
//! `~/.config/dockmate.toml` (or `$XDG_CONFIG_HOME/dockmate.toml`) holds the
//! defaults a user would otherwise retype on every invocation. Command-line
//! flags always win over it, and the environment sits between the two — the
//! ordinary CLI > env > file precedence.
//!
//! The file is a flat list of `key = value` pairs, which is a small enough
//! subset of TOML to parse here rather than take on a parser and serde. What is
//! supported is exactly what the keys below need: booleans, integers, basic
//! (`"…"`) and literal (`'…'`) strings, `#` comments, and blank lines. Anything
//! else — a table header, an array, a float — is rejected by name so the error
//! says what the file can't do instead of quietly ignoring a line.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::ui::theme::{Glyphs, Palette};

/// Every field is optional: absent means "no opinion", and the caller falls
/// back to the flag, the environment, or the built-in default in that order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    pub host: Option<String>,
    pub interval: Option<u64>,
    pub glyphs: Option<Glyphs>,
    pub palette: Option<Palette>,
    pub mouse: Option<bool>,
    pub group_by_stack: Option<bool>,
}

/// What [`load`] found, including anything the user should know about but that
/// isn't worth refusing to start over.
pub struct Loaded {
    pub config: Config,
    /// The file we read, if there was one. Absent means no config file exists,
    /// which is the normal case and not an error.
    pub path: Option<PathBuf>,
    /// Keys we didn't recognise. Surfaced as a toast rather than a hard error:
    /// a typo shouldn't stop the app from starting, but silently doing nothing
    /// is how a setting gets reported as broken.
    pub warnings: Vec<String>,
}

/// Where the config file lives: `$XDG_CONFIG_HOME/dockmate.toml`, falling back
/// to `~/.config/dockmate.toml`.
pub fn path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Some(Path::new(&dir).join("dockmate.toml"));
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(Path::new(&home).join(".config").join("dockmate.toml"))
}

/// Read and parse the config file. A missing file yields the defaults; an
/// unreadable or malformed one is an error, because a config the user wrote and
/// we then ignored is worse than a message saying which line is wrong.
pub fn load() -> Result<Loaded> {
    let Some(path) = path() else {
        return Ok(Loaded {
            config: Config::default(),
            path: None,
            warnings: Vec::new(),
        });
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                config: Config::default(),
                path: None,
                warnings: Vec::new(),
            });
        }
        Err(e) => bail!("reading {}: {e}", path.display()),
    };

    let (config, warnings) =
        parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;

    Ok(Loaded {
        config,
        path: Some(path),
        warnings,
    })
}

/// Parse the file body. Split out from [`load`] so it can be tested without
/// touching the filesystem.
fn parse(text: &str) -> Result<(Config, Vec<String>)> {
    let mut config = Config::default();
    let mut warnings = Vec::new();
    let mut seen: Vec<&str> = Vec::new();

    for (n, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let line_no = n + 1;

        if line.starts_with('[') {
            bail!(
                "line {line_no}: dockmate.toml is a flat file of `key = value` lines; it has no tables"
            );
        }

        let Some((key, value)) = line.split_once('=') else {
            bail!("line {line_no}: expected `key = value`, found `{line}`");
        };
        let key = key.trim();
        let value = value.trim();

        if key.is_empty() {
            bail!("line {line_no}: missing a key before `=`");
        }
        if seen.contains(&key) {
            bail!("line {line_no}: `{key}` is set twice");
        }
        seen.push(key);

        match key {
            "host" => config.host = Some(string(value, line_no)?),
            "interval" => {
                let ms = integer(value, line_no)?;
                // Mirrors the flag's own guard: tokio's interval panics on zero.
                if ms < 1 {
                    bail!("line {line_no}: `interval` must be at least 1 millisecond");
                }
                config.interval = Some(ms);
            }
            "glyphs" => {
                config.glyphs = Some(match string(value, line_no)?.as_str() {
                    "unicode" => Glyphs::Unicode,
                    "nerd" => Glyphs::Nerd,
                    "ascii" => Glyphs::Ascii,
                    other => bail!(
                        "line {line_no}: `glyphs` must be \"unicode\", \"nerd\" or \"ascii\", not \"{other}\""
                    ),
                });
            }
            "palette" => {
                config.palette = Some(match string(value, line_no)?.as_str() {
                    "truecolor" => Palette::TrueColor,
                    "ansi" => Palette::Ansi,
                    "none" => Palette::Mono,
                    other => bail!(
                        "line {line_no}: `palette` must be \"truecolor\", \"ansi\" or \"none\", not \"{other}\""
                    ),
                });
            }
            "mouse" => config.mouse = Some(boolean(value, line_no)?),
            "group_by_stack" => config.group_by_stack = Some(boolean(value, line_no)?),
            other => warnings.push(other.to_string()),
        }
    }

    Ok((config, warnings))
}

/// Drop a trailing `#` comment, leaving `#` inside a quoted value alone.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<u8> = None;
    let mut escaped = false;

    for (i, &b) in line.as_bytes().iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            // Escapes only exist inside a basic string, so a lone backslash in
            // a literal string can't hide the closing quote.
            Some(b'"') if b == b'\\' => escaped = true,
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'#' => return &line[..i],
                _ => {}
            },
        }
    }
    line
}

fn boolean(value: &str, line_no: usize) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => bail!("line {line_no}: expected `true` or `false`, found `{other}`"),
    }
}

fn integer(value: &str, line_no: usize) -> Result<u64> {
    // Underscore separators are TOML's, and `1_000` is a natural way to write
    // an interval.
    let cleaned = value.replace('_', "");
    cleaned
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("line {line_no}: expected a whole number, found `{value}`"))
}

/// A TOML basic (`"…"`) or literal (`'…'`) string. Basic strings honour the
/// four escapes a docker host or a glyph name could plausibly need; literal
/// strings, per TOML, honour none.
fn string(value: &str, line_no: usize) -> Result<String> {
    let bytes = value.as_bytes();
    let quote = match bytes.first() {
        Some(&b'"') => b'"',
        Some(&b'\'') => b'\'',
        _ => bail!("line {line_no}: expected a quoted string, found `{value}`"),
    };
    if bytes.len() < 2 || bytes[bytes.len() - 1] != quote {
        bail!("line {line_no}: unterminated string `{value}`");
    }
    let inner = &value[1..value.len() - 1];

    if quote == b'\'' {
        if inner.contains('\'') {
            bail!("line {line_no}: unterminated string `{value}`");
        }
        return Ok(inner.to_string());
    }

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if c == '"' {
                bail!("line {line_no}: unterminated string `{value}`");
            }
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => bail!("line {line_no}: unknown escape `\\{other}`"),
            None => bail!("line {line_no}: string ends in a backslash"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> Config {
        parse(text).expect("parses").0
    }

    #[test]
    fn an_empty_file_has_no_opinions() {
        assert_eq!(ok(""), Config::default());
        assert_eq!(ok("\n  \n# just a comment\n"), Config::default());
    }

    #[test]
    fn every_key_round_trips() {
        let config = ok(r#"
            host = "tcp://10.0.0.5:2375"
            interval = 1_000
            glyphs = "nerd"
            palette = "ansi"
            mouse = false
            group_by_stack = true
        "#);
        assert_eq!(config.host.as_deref(), Some("tcp://10.0.0.5:2375"));
        assert_eq!(config.interval, Some(1000));
        assert_eq!(config.glyphs, Some(Glyphs::Nerd));
        assert_eq!(config.palette, Some(Palette::Ansi));
        assert_eq!(config.mouse, Some(false));
        assert_eq!(config.group_by_stack, Some(true));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let config = ok("# leading\n\n  mouse = false  # trailing\n\n");
        assert_eq!(config.mouse, Some(false));
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        let config = ok(r##"host = "tcp://host/#frag" # real comment"##);
        assert_eq!(config.host.as_deref(), Some("tcp://host/#frag"));
    }

    #[test]
    fn literal_strings_keep_their_backslashes() {
        let config = ok(r#"host = 'npipe:\\.\pipe\docker_engine'"#);
        assert_eq!(
            config.host.as_deref(),
            Some(r"npipe:\\.\pipe\docker_engine")
        );
    }

    #[test]
    fn basic_strings_honour_escapes() {
        let config = ok(r#"host = "a\\b\"c""#);
        assert_eq!(config.host.as_deref(), Some(r#"a\b"c"#));
    }

    #[test]
    fn unknown_keys_warn_rather_than_fail() {
        let (config, warnings) = parse("colour = \"blue\"\nmouse = true\n").expect("parses");
        assert_eq!(warnings, vec!["colour".to_string()]);
        // The keys we do understand still take effect.
        assert_eq!(config.mouse, Some(true));
    }

    #[test]
    fn bad_values_name_their_line() {
        let err = parse("mouse = true\nglyphs = \"emoji\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("line 2"), "{err}");
        assert!(err.contains("emoji"), "{err}");
    }

    #[test]
    fn a_zero_interval_is_refused() {
        // tokio's interval panics on it, so catch it here rather than there.
        assert!(parse("interval = 0").is_err());
    }

    #[test]
    fn tables_are_refused_by_name() {
        let err = parse("[appearance]\nglyphs = \"ascii\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no tables"), "{err}");
    }

    #[test]
    fn a_duplicate_key_is_an_error() {
        let err = parse("mouse = true\nmouse = false\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("set twice"), "{err}");
    }

    #[test]
    fn unquoted_and_unterminated_strings_are_refused() {
        assert!(parse("glyphs = ascii").is_err());
        assert!(parse("host = \"tcp://x").is_err());
        assert!(parse("host = 'tcp://x").is_err());
    }

    #[test]
    fn a_missing_home_is_not_a_crash() {
        // Only asserts the signature is total; the value depends on the env.
        let _ = path();
    }
}
