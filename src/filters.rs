//! Which names the walk is allowed to look at.
//!
//! The oracle spells all of this with `fnmatch` and `str.rpartition`; the rules
//! below are ports of `dedup.py`'s `file_ext`, `ext_allowed`, `normalize_ext`
//! and `resolve_ext_filters`, and of the exclusion test its two walkers run
//! before they descend.

use std::ffi::OsStr;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub struct Filters<'a> {
    pub min_size: u64,
    pub include_ext: Option<&'a [String]>,
    pub skip_ext: &'a [String],
    pub excludes: &'a [String],
}

#[derive(Debug)]
pub struct FilterError {
    pub message: String,
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FilterError {}

/// One leading dot comes off, not every one: `--ext ..gz` names the extension
/// ".gz", which matches nothing, and the oracle keeps it that way.
pub fn normalize_extension(value: &str) -> String {
    let value = value.trim();
    let value = value.strip_prefix('.').unwrap_or(value);
    value.to_lowercase()
}

fn collect_extensions(values: &[String], flag: &str) -> Result<Vec<String>, FilterError> {
    let mut extensions = Vec::new();
    for raw in values {
        for token in raw.split(',') {
            let extension = normalize_extension(token);
            if extension.is_empty() {
                return Err(FilterError {
                    message: format!("{flag}: empty extension in '{raw}'"),
                });
            }
            extensions.push(extension);
        }
    }
    Ok(extensions)
}

/// The oracle's rule is narrower than "both flags at once is an error": only
/// the same normalized extension appearing in both lists is.
pub fn resolve_extensions(
    ext: &[String],
    skip_ext: &[String],
) -> Result<(Option<Vec<String>>, Vec<String>), FilterError> {
    let include_ext = if ext.is_empty() {
        None
    } else {
        Some(collect_extensions(ext, "--ext")?)
    };
    let exclude_ext = collect_extensions(skip_ext, "--skip-ext")?;
    if let Some(include_ext) = &include_ext {
        let mut conflicts = include_ext
            .iter()
            .filter(|extension| exclude_ext.contains(extension))
            .cloned()
            .collect::<Vec<_>>();
        conflicts.sort();
        conflicts.dedup();
        if !conflicts.is_empty() {
            return Err(FilterError {
                message: format!("in both --ext and --skip-ext: {}", conflicts.join(", ")),
            });
        }
    }
    Ok((include_ext, exclude_ext))
}

/// The final suffix, lowercased, no dot. A dotfile with no other dot has none.
pub fn file_extension(path: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_lowercase(),
        _ => String::new(),
    }
}

pub fn extension_allowed(name: &str, filters: &Filters) -> bool {
    let extension = file_extension(name);
    if let Some(include) = filters.include_ext {
        if !include.iter().any(|allowed| allowed == &extension) {
            return false;
        }
    }
    !filters.skip_ext.iter().any(|skipped| skipped == &extension)
}

pub fn excluded(relative: &str, filters: &Filters) -> bool {
    let path = Path::new(relative);
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    filters.excludes.iter().any(|pattern| {
        glob_match(pattern, name)
            || path.parent().is_some_and(|directory| {
                directory.components().any(|component| {
                    glob_match(pattern, component.as_os_str().to_str().unwrap_or_default())
                })
            })
    })
}

/// `fnmatch`'s pattern language: `*`, `?`, `[seq]`, `[!seq]` and ranges. An
/// unterminated `[` is a literal bracket, which is what fnmatch does with it.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    #[derive(Clone)]
    enum Token {
        Any,
        One,
        Set {
            characters: Vec<char>,
            ranges: Vec<(char, char)>,
            negated: bool,
        },
    }

    fn parse(pattern: &str) -> Vec<Token> {
        let characters = pattern.chars().collect::<Vec<_>>();
        let mut index = 0;
        let mut tokens = Vec::new();
        while index < characters.len() {
            match characters[index] {
                '*' => {
                    tokens.push(Token::Any);
                    index += 1;
                }
                '?' => {
                    tokens.push(Token::One);
                    index += 1;
                }
                '[' => {
                    let start = index + 1;
                    let mut cursor = start;
                    let negated = matches!(characters.get(cursor), Some('!'));
                    if negated {
                        cursor += 1;
                    }
                    // A ']' in first position is literal, matching fnmatch.
                    let mut set = Vec::new();
                    let mut ranges = Vec::new();
                    let mut closed = false;
                    while cursor < characters.len() {
                        if characters[cursor] == ']' && cursor > start {
                            closed = true;
                            break;
                        }
                        let first = characters[cursor];
                        if first == ']' {
                            set.push(first);
                            cursor += 1;
                            continue;
                        }
                        if matches!(characters.get(cursor + 1), Some('-'))
                            && matches!(characters.get(cursor + 2), Some(&second) if second != ']')
                        {
                            let second = characters[cursor + 2];
                            ranges.push((first.min(second), first.max(second)));
                            cursor += 3;
                            continue;
                        }
                        set.push(first);
                        cursor += 1;
                    }
                    if closed {
                        tokens.push(Token::Set {
                            characters: set,
                            ranges,
                            negated,
                        });
                        index = cursor + 1;
                    } else {
                        tokens.push(Token::Set {
                            characters: vec!['['],
                            ranges: Vec::new(),
                            negated: false,
                        });
                        index += 1;
                    }
                }
                literal => {
                    tokens.push(Token::Set {
                        characters: vec![literal],
                        ranges: Vec::new(),
                        negated: false,
                    });
                    index += 1;
                }
            }
        }
        tokens
    }

    fn matches_set(token: &Token, actual: char) -> bool {
        let Token::Set {
            characters,
            ranges,
            negated,
        } = token
        else {
            return false;
        };
        let contained = characters.contains(&actual)
            || ranges
                .iter()
                .any(|(start, end)| actual >= *start && actual <= *end);
        contained != *negated
    }

    // Whether tokens[t..] matches text[c..], filled bottom-up.
    //
    // Recursing on `*` instead — trying every split point and letting each try
    // recurse again — is exponential: `*a*a*a*a*b` against forty a's took two
    // minutes here, against fnmatch's third of a second, because fnmatch
    // compiles to a regex and a regex does not re-derive the same subproblem
    // twice. Neither does this: each (token, position) pair is decided once,
    // which makes the whole match O(tokens x characters). A `-x` pattern comes
    // from the user and gets pointed at large trees, so its cost has to be a
    // function of the input rather than of the input's shape.
    let tokens = parse(pattern);
    let text: Vec<char> = text.chars().collect();
    let width = text.len() + 1;
    let mut matches = vec![false; (tokens.len() + 1) * width];
    matches[tokens.len() * width + text.len()] = true; // both exhausted

    for t in (0..tokens.len()).rev() {
        for c in (0..=text.len()).rev() {
            let here = match &tokens[t] {
                // `*` either stands for nothing, or eats one more character.
                Token::Any => {
                    matches[(t + 1) * width + c] || (c < text.len() && matches[t * width + c + 1])
                }
                Token::One => c < text.len() && matches[(t + 1) * width + c + 1],
                set => {
                    c < text.len() && matches_set(set, text[c]) && matches[(t + 1) * width + c + 1]
                }
            };
            matches[t * width + c] = here;
        }
    }
    matches[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters<'a>(excludes: &'a [String], skip_ext: &'a [String]) -> Filters<'a> {
        Filters {
            min_size: 1,
            include_ext: None,
            skip_ext,
            excludes,
        }
    }

    #[test]
    fn extension_matching_uses_final_suffix_and_dotfiles_have_none() {
        assert_eq!(file_extension("photo.JPG"), "jpg");
        assert_eq!(file_extension(".hidden"), "");
        assert_eq!(file_extension("archive.tar.gz"), "gz");
    }

    #[test]
    fn normalization_strips_whitespace_and_exactly_one_leading_dot() {
        assert_eq!(normalize_extension(" JPG "), "jpg");
        assert_eq!(normalize_extension(".gz"), "gz");
        assert_eq!(normalize_extension("..gz"), ".gz");
    }

    #[test]
    fn extension_filter_conflicts_and_empty_tokens_are_errors() {
        let error = resolve_extensions(&["gz".to_string()], &[".GZ".to_string()])
            .expect_err("same normalized extension must conflict");
        assert_eq!(error.message, "in both --ext and --skip-ext: gz");

        let error = resolve_extensions(&[], &["jpg,,gz".to_string()])
            .expect_err("empty token must be rejected");
        assert_eq!(error.message, "--skip-ext: empty extension in 'jpg,,gz'");

        let (include, exclude) = resolve_extensions(&["jpg".to_string()], &["tmp".to_string()])
            .expect("different extensions are legal");
        assert_eq!(include.as_deref(), Some(&["jpg".to_string()][..]));
        assert_eq!(exclude, ["tmp".to_string()]);
    }

    #[test]
    fn glob_supports_character_classes_ranges_and_negation() {
        assert!(glob_match("[ab].gz", "a.gz"));
        assert!(glob_match("[ab].gz", "b.gz"));
        assert!(!glob_match("[ab].gz", "c.gz"));
        assert!(glob_match("[!x]*.gz", "abc.gz"));
        assert!(!glob_match("[!x]*.gz", "x.gz"));
        assert!(glob_match("[a-z].bin", "q.bin"));
        assert!(!glob_match("[a-z].bin", "Q.bin"));
        assert!(glob_match("[[]literal]", "[literal]"));
        assert!(glob_match("literal[", "literal["));
    }

    #[test]
    fn a_pathological_pattern_still_finishes_promptly() {
        // The shape that used to backtrack exponentially: many `*x*` groups
        // against a long run of the same character, with a final literal that
        // can never match, so every split point is explored. REVIEW-M2 measured
        // 121 seconds here; bottom-up it is a few hundred microseconds.
        let name = "a".repeat(40) + ".bin";
        let start = std::time::Instant::now();
        assert!(!glob_match("*a*a*a*a*a*a*a*a*a*a*a*b", &name));
        assert!(glob_match("*a*a*a*a*a*a*a*a*a*a*a*n", &name));
        assert!(!glob_match(&"*a".repeat(30), &name));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "pathological patterns took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_star_matches_across_and_at_the_ends() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("**", "anything"));
        assert!(glob_match("a*", "a"));
        assert!(glob_match("*a", "a"));
        assert!(glob_match("a*c", "abbbc"));
        assert!(!glob_match("a*c", "abbbd"));
        assert!(!glob_match("?", ""));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "a"));
    }

    #[test]
    fn exclusion_matches_entry_or_any_directory_component() {
        let empty = Vec::new();
        let patterns = vec!["*.tmp".to_string()];
        let temporary = filters(&patterns, &empty);
        assert!(!excluded("file", &temporary));
        assert!(excluded("backup.tmp/file", &temporary));

        let directory_patterns = vec!["backup".to_string()];
        let backup = filters(&directory_patterns, &empty);
        assert!(excluded("backup/report.bin", &backup));
        assert!(excluded("nested/backup/report.bin", &backup));
        assert!(!excluded("backups/report.bin", &backup));
    }
}
