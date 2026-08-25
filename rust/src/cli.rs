//! The command line: one table, and everything that reads it.
//!
//! `FLAGS` is the single source of flag truth. The parser walks it to decide
//! what each argument means, `--help` renders it, and the pre-flight menu asks
//! it for its fields and their defaults. Nothing here spells a flag twice, so
//! nothing here can drift: a flag added to the table appears in all three at
//! once, and a flag the table does not carry is accepted by none of them.
//!
//! Values become options in exactly one place — `Flag::apply` — so the menu
//! validating a field and the parser reading an argument are running the same
//! code and produce the same message.

use std::path::PathBuf;

use crate::consts::DEFAULT_MIN_SIZE;
use crate::device::Wanted;
use crate::size::{exact_size, parse_size};

#[derive(Default, Clone, Debug)]
pub struct Options {
    pub root: PathBuf,
    pub report_json: Option<PathBuf>,
    pub min_size: u64,
    pub jobs: Option<usize>,
    pub excludes: Vec<String>,
    pub ext: Vec<String>,
    pub skip_ext: Vec<String>,
    pub list: bool,
    /// Ask for the browser outright. Without it the browser is what a terminal
    /// gets when nothing else was asked for, which is the oracle's behaviour;
    /// with it, no terminal is an error rather than a silent fallback.
    pub interactive: bool,
    pub auto: Option<String>,
    pub yes: bool,
    pub hard: bool,
    pub no_cache: bool,
    /// Read no verdicts from the cache this run, but still write what the
    /// scan learns. Distinct from `no_cache`, which neither reads nor writes.
    pub force: bool,
    /// No progress line, whatever stderr turns out to be attached to.
    pub quiet: bool,
    /// "size" stops after the walk and size grouping. Only the M1 gate asks
    /// for it; every other gate leaves it empty.
    pub stage: String,
    pub profile: Wanted,
}

impl Options {
    pub fn new() -> Options {
        Options {
            min_size: DEFAULT_MIN_SIZE,
            ..Options::default()
        }
    }
}

pub const KEEP_RULES: [&str; 3] = ["shortest", "oldest", "newest"];

/// What follows a flag on the command line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Takes {
    /// Nothing: the flag is on or off.
    Nothing,
    /// A value, named here for the help text and the menu's prompt.
    Value(&'static str),
    /// A value, but only when the next word is one of these — `--auto` alone
    /// means `--auto shortest`, and `--auto -y` is not a rule called `-y`.
    OneOf(&'static str, &'static [&'static str]),
}

pub struct Flag {
    pub long: &'static str,
    pub short: Option<&'static str>,
    pub takes: Takes,
    pub help: &'static str,
    /// What the tool does without the flag, in the words `--help` should use.
    /// Checked against the real default in the tests below, because a help
    /// text that describes a default the code does not have is worse than one
    /// that says nothing.
    pub default: &'static str,
}

impl Flag {
    /// Every spelling this flag answers to.
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        self.short.into_iter().chain(std::iter::once(self.long))
    }

    /// Fold this flag into the options, with the value it was given.
    ///
    /// The one place a word from a user becomes a setting. The menu validates
    /// a field by calling it on a scratch `Options`, so a value the menu
    /// accepts is a value the parser accepts, with the same message when it is
    /// not.
    pub fn apply(&self, options: &mut Options, value: Option<&str>) -> Result<(), String> {
        let need = || -> Result<&str, String> {
            value.ok_or_else(|| format!("{} needs a value", self.long))
        };
        match self.long {
            "--report-json" => options.report_json = Some(PathBuf::from(need()?)),
            "--min-size" => options.min_size = parse_size(need()?)?,
            "--jobs" => {
                let jobs: usize = need()?
                    .parse()
                    .map_err(|_| "--jobs takes a whole number of readers".to_string())?;
                if jobs == 0 {
                    return Err("--jobs takes at least one reader".to_string());
                }
                options.jobs = Some(jobs);
            }
            "--exclude" => options.excludes.push(need()?.to_string()),
            "--ext" => options.ext.push(need()?.to_string()),
            "--skip-ext" => options.skip_ext.push(need()?.to_string()),
            "--stage" => {
                let stage = need()?;
                if stage != "size" && stage != "full" {
                    return Err("--stage takes size or full".to_string());
                }
                options.stage = if stage == "full" {
                    String::new()
                } else {
                    stage.to_string()
                };
            }
            "--profile" => {
                options.profile = Wanted::parse(need()?)
                    .ok_or_else(|| "--profile takes flash, spinning or auto".to_string())?
            }
            "--auto" => options.auto = Some(value.unwrap_or("shortest").to_string()),
            "--list" => options.list = true,
            "--interactive" => options.interactive = true,
            "--yes" => options.yes = true,
            "--hard" => options.hard = true,
            "--no-cache" => options.no_cache = true,
            "--force" => options.force = true,
            "--quiet" => options.quiet = true,
            // Accepted so a command line written for the Python tool still
            // runs. This build verifies every group it deletes from
            // regardless, so there is nothing for the flag to turn on.
            "--verify" => {}
            other => return Err(format!("{other} is in the flag table and nowhere else")),
        }
        Ok(())
    }

    /// A value for this flag, in the words the menu prefills its field with.
    ///
    /// `apply`'s inverse, and bound to it by test: what comes out here has to
    /// go back in there as the same setting. A size is the one value worth
    /// rewriting — 10485760 typed on the command line comes back as `10 MiB`,
    /// which is what the form then hands the parser, so the row a person reads
    /// and the row the scan runs on are the same row. Anything the grammar
    /// cannot read is echoed untouched; the parser has already accepted every
    /// value that reaches here, so that is a fallback rather than a path.
    pub fn show(&self, value: &str) -> String {
        match self.long {
            "--min-size" => parse_size(value).map_or_else(|_| value.to_string(), exact_size),
            _ => value.to_string(),
        }
    }
}

/// The table. Order is the order `--help` prints them in: what to scan, then
/// what to scan it with, then what to do about what it finds.
pub const FLAGS: &[Flag] = &[
    Flag {
        long: "--min-size",
        short: None,
        takes: Takes::Value("SIZE"),
        help: "ignore files smaller than this; plain digits are bytes, or say \
               512k, 100mb, 1.5g, 2TiB — all binary multiples",
        default: "10 MiB",
    },
    Flag {
        long: "--ext",
        short: None,
        takes: Takes::Value("LIST"),
        help: "only these extensions, comma-separated; repeatable",
        default: "every extension",
    },
    Flag {
        long: "--skip-ext",
        short: None,
        takes: Takes::Value("LIST"),
        help: "skip these extensions, comma-separated; repeatable",
        default: "skip nothing",
    },
    Flag {
        long: "--exclude",
        short: Some("-x"),
        takes: Takes::Value("GLOB"),
        help: "skip paths matching this glob; repeatable",
        default: "exclude nothing",
    },
    Flag {
        long: "--jobs",
        short: Some("-j"),
        takes: Takes::Value("N"),
        help: "readers to use",
        default: "as many as the disk is worth",
    },
    Flag {
        long: "--profile",
        short: None,
        takes: Takes::Value("NAME"),
        help: "access patterns to use: flash, spinning or auto",
        default: "auto",
    },
    Flag {
        long: "--no-cache",
        short: None,
        takes: Takes::Nothing,
        help: "neither read nor write the scan cache",
        default: "use the scan cache",
    },
    Flag {
        long: "--force",
        short: None,
        takes: Takes::Nothing,
        help: "read nothing the cache remembers, but write what this scan learns",
        default: "use the scan cache",
    },
    Flag {
        long: "--list",
        short: Some("-l"),
        takes: Takes::Nothing,
        help: "print the duplicate groups and stop",
        default: "browse them, given a terminal",
    },
    Flag {
        long: "--interactive",
        short: Some("-i"),
        takes: Takes::Nothing,
        help: "browse them, and fail if there is no terminal",
        default: "browse only when a terminal is there",
    },
    Flag {
        long: "--auto",
        short: None,
        takes: Takes::OneOf("RULE", &KEEP_RULES),
        help: "delete without asking, keeping shortest, oldest or newest",
        default: "delete nothing",
    },
    Flag {
        long: "--yes",
        short: Some("-y"),
        takes: Takes::Nothing,
        help: "do not ask before deleting",
        default: "ask",
    },
    Flag {
        long: "--hard",
        short: None,
        takes: Takes::Nothing,
        help: "delete permanently instead of moving to the Trash",
        default: "move to the Trash",
    },
    Flag {
        long: "--report-json",
        short: None,
        takes: Takes::Value("PATH"),
        help: "write the scan report here",
        default: "a file in the temporary directory",
    },
    Flag {
        long: "--stage",
        short: None,
        takes: Takes::Value("NAME"),
        help: "size, to stop after grouping by length and report that; \
               nothing there has been read, so --auto refuses it",
        default: "full",
    },
    Flag {
        long: "--quiet",
        short: Some("-q"),
        takes: Takes::Nothing,
        help: "no progress line while the scan runs",
        default: "a progress line, on a terminal",
    },
    Flag {
        long: "--verify",
        short: None,
        takes: Takes::Nothing,
        help: "accepted for the Python tool's sake; ignored here, which \
               re-proves every group it deletes from anyway",
        default: "ignored either way",
    },
];

pub const HELP_FLAGS: [&str; 2] = ["-h", "--help"];

pub fn flag_named(name: &str) -> Option<&'static Flag> {
    FLAGS
        .iter()
        .find(|flag| flag.names().any(|spelling| spelling == name))
}

/// What a command line turned out to be.
pub enum Parsed {
    Scan(Box<Options>),
    Help,
}

/// Read a command line against the table.
pub fn parse_arguments_from<I: Iterator<Item = String>>(arguments: I) -> Result<Parsed, String> {
    let mut options = Options::new();
    let mut root = None;
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        if HELP_FLAGS.contains(&argument.as_str()) {
            return Ok(Parsed::Help);
        }
        match flag_named(&argument) {
            Some(flag) => {
                let value = match flag.takes {
                    Takes::Nothing => None,
                    Takes::Value(_) => Some(
                        arguments
                            .next()
                            .ok_or_else(|| format!("{argument} needs a value"))?,
                    ),
                    // Only a word from the list is this flag's value; anything
                    // else is the next argument.
                    Takes::OneOf(_, allowed) => arguments
                        .peek()
                        .is_some_and(|next| allowed.contains(&next.as_str()))
                        .then(|| arguments.next().expect("peeked a value")),
                };
                flag.apply(&mut options, value.as_deref())?;
            }
            None if root.is_none() && !argument.starts_with('-') => {
                root = Some(PathBuf::from(argument));
            }
            None => return Err(format!("unknown argument: {argument}")),
        }
    }
    // The oracle runs on the current directory when no path is given.
    options.root = root.unwrap_or_else(|| PathBuf::from("."));
    Ok(Parsed::Scan(Box::new(options)))
}

/// How a flag is written in the help text and in the menu's field list.
pub fn spelling(flag: &Flag) -> String {
    let names = match flag.short {
        Some(short) => format!("{short}, {}", flag.long),
        None => format!("    {}", flag.long),
    };
    match flag.takes {
        Takes::Nothing => names,
        Takes::Value(name) => format!("{names} {name}"),
        Takes::OneOf(name, _) => format!("{names} [{name}]"),
    }
}

/// Break `text` into lines that fit `width`, without splitting a word.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        let line = lines.last_mut().expect("there is always one");
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(word.to_string());
        }
    }
    lines
}

/// `--help`, rendered from the table and from nowhere else.
pub fn help_text() -> String {
    let mut out = String::from(
        "dedup-rs — find byte-exact duplicate files.\n\n\
         usage: dedup-rs [root] [options]\n\n\
         Groups of files whose bytes are identical, largest first. Nothing is\n\
         ever called a duplicate on the strength of a hash: every group is\n\
         read and compared. Runs on the current directory when no root is\n\
         given. Without --list or --auto, a terminal gets the interactive\n\
         browser.\n\noptions:\n",
    );
    let widest = FLAGS
        .iter()
        .map(|flag| spelling(flag).chars().count())
        .max()
        .unwrap_or(0);
    // Two columns, and the right one wraps: a terminal 80 wide is the one
    // this is read in.
    let indent = " ".repeat(widest + 4);
    let room = 78_usize.saturating_sub(widest + 4).max(24);
    for flag in FLAGS {
        let mut said = wrapped(flag.help, room);
        said.extend(wrapped(&format!("(default: {})", flag.default), room));
        for (line, text) in said.iter().enumerate() {
            if line == 0 {
                out.push_str(&format!("  {:widest$}  {text}\n", spelling(flag)));
            } else {
                out.push_str(&format!("{indent}{text}\n"));
            }
        }
    }
    out.push_str(&format!(
        "  {:widest$}  {}\n",
        HELP_FLAGS.join(", "),
        "print this and stop",
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(words: &[&str]) -> Result<Options, String> {
        match parse_arguments_from(words.iter().map(|word| word.to_string()))? {
            Parsed::Scan(options) => Ok(*options),
            Parsed::Help => Err("help".to_string()),
        }
    }

    #[test]
    fn no_root_means_the_current_directory() {
        // Parity with the oracle: "Runs on the current directory when no
        // path is given."
        assert_eq!(parse(&[]).unwrap().root, PathBuf::from("."));
        assert_eq!(parse(&["--list"]).unwrap().root, PathBuf::from("."));
    }

    /// Every flag token the help text mentions.
    fn flags_in_help() -> Vec<String> {
        let text = help_text();
        let mut found: Vec<String> = Vec::new();
        for word in text.split(|c: char| c.is_whitespace() || c == ',') {
            let word = word.trim_end_matches(['.', ':', ';']);
            if word.starts_with('-') && word.len() > 1 && !found.iter().any(|s| s == word) {
                found.push(word.to_string());
            }
        }
        found.sort();
        found
    }

    #[test]
    fn the_help_text_and_the_parser_know_exactly_the_same_flags() {
        // The one-table rule, enforced in both directions. A flag added to the
        // parser but not the help, or the other way round, fails here.
        let mut from_table: Vec<String> = FLAGS
            .iter()
            .flat_map(|flag| flag.names())
            .chain(HELP_FLAGS)
            .map(str::to_string)
            .collect();
        from_table.sort();
        assert_eq!(flags_in_help(), from_table);

        for name in &from_table {
            assert!(
                flag_named(name).is_some() || HELP_FLAGS.contains(&name.as_str()),
                "{name} is in the help text but the parser does not know it"
            );
        }
    }

    #[test]
    fn every_flag_in_the_table_is_accepted_and_nothing_else_is() {
        for flag in FLAGS {
            for name in flag.names() {
                let mut words = vec!["/tmp", name];
                match flag.takes {
                    Takes::Nothing => {}
                    Takes::Value("SIZE") | Takes::Value("N") => words.push("1"),
                    Takes::Value("NAME") if flag.long == "--profile" => words.push("flash"),
                    Takes::Value("NAME") => words.push("full"),
                    Takes::Value(_) => words.push("x"),
                    Takes::OneOf(_, allowed) => words.push(allowed[0]),
                }
                assert!(parse(&words).is_ok(), "the parser rejected {name}");
            }
        }
        assert_eq!(
            parse(&["/tmp", "--turbo"]).unwrap_err(),
            "unknown argument: --turbo"
        );
    }

    #[test]
    fn the_defaults_the_help_text_prints_are_the_defaults_the_code_has() {
        // The failure this catches is a help text that describes a default
        // somebody changed in the code and not here.
        let bare = parse(&["/tmp"]).expect("a root is enough");
        let said = |long: &str| flag_named(long).expect("in the table").default;
        // Shown humanly, and shown as the very words that would set it — a
        // default nobody could type back in is a default that has drifted.
        assert_eq!(said("--min-size"), "10 MiB");
        assert_eq!(said("--min-size"), exact_size(bare.min_size));
        assert_eq!(parse_size(said("--min-size")), Ok(bare.min_size));
        assert_eq!(bare.min_size, DEFAULT_MIN_SIZE);
        assert_eq!(said("--profile"), "auto");
        assert_eq!(bare.profile, Wanted::Auto);
        assert_eq!(bare.jobs, None);
        assert!(bare.ext.is_empty() && bare.skip_ext.is_empty() && bare.excludes.is_empty());
        assert!(!bare.list && !bare.interactive && !bare.yes && !bare.hard && !bare.no_cache);
        assert!(!bare.quiet);
        assert_eq!(bare.auto, None);
        assert_eq!(bare.stage, "");
        assert_eq!(bare.report_json, None);
    }

    #[test]
    fn help_is_asked_for_by_either_spelling_and_beats_everything_else() {
        for spelling in HELP_FLAGS {
            assert!(matches!(
                parse_arguments_from([spelling.to_string()].into_iter()),
                Ok(Parsed::Help)
            ));
            // Even with no root, which used to be the error this printed.
            assert!(matches!(
                parse_arguments_from(
                    [
                        "/tmp".to_string(),
                        spelling.to_string(),
                        "--nonsense".to_string()
                    ]
                    .into_iter()
                ),
                Ok(Parsed::Help)
            ));
        }
    }

    #[test]
    fn the_optional_rule_after_auto_is_taken_only_when_it_is_a_rule() {
        assert_eq!(
            parse(&["/tmp", "--auto"]).unwrap().auto.unwrap(),
            "shortest"
        );
        assert_eq!(
            parse(&["/tmp", "--auto", "oldest"]).unwrap().auto.unwrap(),
            "oldest"
        );
        let with_flag = parse(&["/tmp", "--auto", "-y"]).unwrap();
        assert_eq!(with_flag.auto.unwrap(), "shortest");
        assert!(with_flag.yes);
    }

    #[test]
    fn the_repeatable_flags_accumulate() {
        let options = parse(&[
            "/tmp",
            "-x",
            "one",
            "-x",
            "two",
            "--ext",
            "jpg",
            "--skip-ext",
            "tmp",
        ])
        .unwrap();
        assert_eq!(options.excludes, ["one", "two"]);
        assert_eq!(options.ext, ["jpg"]);
        assert_eq!(options.skip_ext, ["tmp"]);
    }

    #[test]
    fn a_value_the_parser_refuses_says_why() {
        assert_eq!(
            parse(&["/tmp", "--min-size", "12qb"]).unwrap_err(),
            "not a size: \"12qb\" — plain bytes or 512k, 100mb, 1.5g"
        );
        assert_eq!(
            parse(&["/tmp", "--profile", "rotational"]).unwrap_err(),
            "--profile takes flash, spinning or auto"
        );
        assert_eq!(
            parse(&["/tmp", "--jobs", "0"]).unwrap_err(),
            "--jobs takes at least one reader"
        );
        assert_eq!(
            parse(&["/tmp", "--stage", "middle"]).unwrap_err(),
            "--stage takes size or full"
        );
        assert_eq!(
            parse(&["/tmp", "--min-size"]).unwrap_err(),
            "--min-size needs a value"
        );
    }

    #[test]
    fn the_command_line_takes_a_size_the_way_a_person_writes_one() {
        // The grammar reached through the table, which is the only way the
        // menu and --help can reach it either.
        let bytes = |word: &str| parse(&["/tmp", "--min-size", word]).map(|o| o.min_size);
        assert_eq!(bytes("10485760"), Ok(10 * 1024 * 1024));
        assert_eq!(bytes("100mb"), Ok(100 * 1024 * 1024));
        assert_eq!(bytes("512k"), Ok(512 * 1024));
        assert_eq!(bytes("1.5g"), Ok(1024 * 1024 * 1024 + 1024 * 1024 * 512));
        assert_eq!(bytes("2TiB"), Ok(2 * 1024_u64.pow(4)));
        assert_eq!(bytes("10 MiB"), Ok(10 * 1024 * 1024));
        assert_eq!(bytes("0"), Ok(0));
    }

    #[test]
    fn what_the_form_shows_is_what_the_parser_reads() {
        // The display round-trip law, at the seam where it can break: the menu
        // prefills a field with `show` and hands `apply` back what it showed.
        // A rounded `show` would quietly change the scan somebody agreed to.
        let min_size = flag_named("--min-size").expect("in the table");
        for word in [
            "0", "1", "1023", "1024", "10485760", "10485761", "1500000", "100mb", "1.5g", "2TiB",
            "10 MiB",
        ] {
            let (mut typed, mut shown) = (Options::new(), Options::new());
            min_size.apply(&mut typed, Some(word)).expect("a size");
            let echo = min_size.show(word);
            min_size.apply(&mut shown, Some(&echo)).expect("a size");
            assert_eq!(
                typed.min_size, shown.min_size,
                "{word:?} was shown as {echo:?}"
            );
        }
        // Every other flag is echoed exactly, so no field can be rewritten by
        // a rule nobody wrote for it.
        for flag in FLAGS.iter().filter(|flag| flag.long != "--min-size") {
            assert_eq!(flag.show("whatever"), "whatever", "{}", flag.long);
        }
    }

    #[test]
    fn quiet_is_a_setting_now_and_verify_is_still_not() {
        assert!(parse(&["/tmp", "--quiet"]).unwrap().quiet);
        assert!(parse(&["/tmp", "-q"]).unwrap().quiet);
        assert!(!parse(&["/tmp", "--verify"]).unwrap().quiet);
    }

    #[test]
    fn the_help_text_says_something_about_every_flag() {
        let text = help_text();
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for flag in FLAGS {
            let help: String = flag.help.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(flat.contains(&help), "{} has no line", flag.long);
            assert!(
                flat.contains(&format!("(default: {})", flag.default)),
                "{} does not say its default",
                flag.long
            );
        }
    }

    #[test]
    fn no_help_line_runs_past_a_terminal_eighty_wide() {
        for line in help_text().lines() {
            assert!(
                line.chars().count() <= 80,
                "{} characters: {line}",
                line.chars().count()
            );
        }
    }
}
