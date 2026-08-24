//! Numbers as a person reads them, and — for sizes — as a person writes them.
//!
//! One grammar, in `parse_size`, and the flag table is the only thing that
//! parses with it: `Flag::apply` is its single caller, so the command line,
//! the pre-flight menu and `--help` are all reading the same code and cannot
//! drift apart. `Flag::show` is its inverse, and `exact_size` is what that
//! inverse is built from.
//!
//! Multiples are binary throughout — `k` is 1024, not 1000 — because binary is
//! what the tool displays. A size read off the screen and typed back in has to
//! mean the same thing, or the browser and the form are describing different
//! scans.

/// The units, smallest first. `human_size` climbs this ladder and `exact_size`
/// climbs it as far as it can go without rounding.
const LADDER: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// The bar `--help` and the rejection message teach. `p` and `e` are read too,
/// so that the whole ladder the browser shows sizes on is a ladder this reads
/// back; nobody types them.
const TAUGHT: &str = "plain bytes or 512k, 100mb, 1.5g";

/// Bytes from a word a person wrote.
///
/// Plain digits are plain bytes, as they always were — the conformance
/// templates are made of them. A suffix multiplies: `k`/`m`/`g`/`t`/`p`/`e`,
/// case-insensitive, alone or dressed as `kb` or `kib`, with at most one space
/// in front of it so that every form the tool displays parses back. Decimals
/// are allowed and floor to whole bytes.
pub fn parse_size(text: &str) -> Result<u64, String> {
    read(text.trim()).ok_or_else(|| format!("not a size: {text:?} — {TAUGHT}"))
}

fn read(text: &str) -> Option<u64> {
    let end = text
        .find(|letter: char| !letter.is_ascii_digit() && letter != '.')
        .unwrap_or(text.len());
    let (number, rest) = text.split_at(end);
    let multiplier = multiplier_of(rest.strip_prefix(' ').unwrap_or(rest))?;

    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    // `1.2.3` leaves the second dot in the fraction, and `.` on its own leaves
    // no digits at all.
    if fraction.contains('.') || (whole.is_empty() && fraction.is_empty()) {
        return None;
    }
    let mut scaled: u128 = 0;
    for digit in whole.bytes().chain(fraction.bytes()) {
        scaled = scaled
            .checked_mul(10)?
            .checked_add(u128::from(digit - b'0'))?;
    }
    let tenth_power = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    u64::try_from(scaled.checked_mul(multiplier)? / tenth_power).ok()
}

/// What a suffix multiplies by, or `None` when it is not one.
fn multiplier_of(suffix: &str) -> Option<u128> {
    let suffix = suffix.to_ascii_lowercase();
    // No letter, or a bare `b`: the number was already bytes.
    if suffix.is_empty() || suffix == "b" {
        return Some(1);
    }
    // By character and not by byte: whatever a person typed lands here, and
    // splitting `€` down the middle is a panic rather than a refusal.
    let mut letters = suffix.chars();
    let letter = letters.next()?;
    if !matches!(letters.as_str(), "" | "b" | "ib") {
        return None;
    }
    let step = "kmgtpe".find(letter)? + 1;
    Some(1024_u128.pow(u32::try_from(step).ok()?))
}

/// Bytes as a person reads them: rounded, for a number nobody is going to type
/// back in.
pub fn human_size(bytes: u64) -> String {
    let (value, unit) = climbed(bytes);
    match unit {
        None => format!("{bytes} B"),
        Some(unit) if value < 100.0 => format!("{value:.1} {unit}"),
        Some(unit) => format!("{value:.0} {unit}"),
    }
}

/// How far up the ladder `bytes` reaches, and what it is worth there. `None`
/// is the bottom rung, where bytes are their own unit.
fn climbed(bytes: u64) -> (f64, Option<&'static str>) {
    let mut value = bytes as f64;
    let mut reached = None;
    for unit in LADDER {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        reached = Some(unit);
    }
    (value, reached)
}

/// Bytes as a person would type them, and as `parse_size` reads back
/// unchanged.
///
/// The largest unit that divides them to a single decimal place — all
/// `parse_size` promises to carry through a decimal point — and plain bytes
/// when no unit does. Exactness is the point: a settings field that rounded
/// what it was handed would be describing a scan the tool is not about to run.
pub fn exact_size(bytes: u64) -> String {
    let mut shown = bytes.to_string();
    let mut unit_bytes: u128 = 1;
    for unit in LADDER {
        unit_bytes *= 1024;
        if u128::from(bytes) < unit_bytes {
            break;
        }
        let tenths = u128::from(bytes) * 10;
        if !tenths.is_multiple_of(unit_bytes) {
            continue;
        }
        let value = tenths / unit_bytes;
        shown = match value % 10 {
            0 => format!("{} {unit}", value / 10),
            tenth => format!("{}.{tenth} {unit}", value / 10),
        };
    }
    shown
}

pub fn human_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: u64 = 1024;
    const M: u64 = 1024 * K;
    const G: u64 = 1024 * M;
    const T: u64 = 1024 * G;

    #[test]
    fn the_grammar_reads_what_the_table_says_it_reads() {
        let cases: &[(&str, u64)] = &[
            // Plain digits stay plain bytes: the conformance templates are
            // written in them and none of this may move them.
            ("0", 0),
            ("1", 1),
            ("10485760", 10 * M),
            ("18446744073709551615", u64::MAX),
            // Every suffix letter, undressed, dressed and half-dressed.
            ("512k", 512 * K),
            ("512kb", 512 * K),
            ("512kib", 512 * K),
            ("100m", 100 * M),
            ("100mb", 100 * M),
            ("100mib", 100 * M),
            ("2g", 2 * G),
            ("2gb", 2 * G),
            ("2gib", 2 * G),
            ("3t", 3 * T),
            ("3tb", 3 * T),
            ("3tib", 3 * T),
            // Case is not a unit of measurement.
            ("2TiB", 2 * T),
            ("2TIB", 2 * T),
            ("512K", 512 * K),
            ("1MiB", M),
            ("1Mb", M),
            // Decimals, floored to whole bytes.
            ("1.5g", G + G / 2),
            ("1.5k", 1536),
            ("0.5m", M / 2),
            ("1.25k", 1280),
            ("0.0009765625k", 1),
            ("1.9k", 1945),
            // One optional space, so everything the tool displays parses back.
            ("10 MiB", 10 * M),
            ("1.5 KiB", 1536),
            ("0 B", 0),
            ("1023 B", 1023),
            // ...and the whole word may be padded, because a form is typed in.
            ("  100mb  ", 100 * M),
            // Zero of anything is still zero.
            ("0k", 0),
            ("0.0g", 0),
        ];
        for &(text, bytes) in cases {
            assert_eq!(parse_size(text), Ok(bytes), "{text:?}");
        }
    }

    #[test]
    fn the_rejection_teaches() {
        assert_eq!(
            parse_size("12qb").unwrap_err(),
            "not a size: \"12qb\" — plain bytes or 512k, 100mb, 1.5g"
        );
    }

    #[test]
    fn junk_negatives_and_overflow_are_all_refused() {
        let refused = [
            // junk
            "12qb",
            "abc",
            "",
            "  ",
            "k",
            "mb",
            "1.2.3",
            ".",
            "1..2",
            "1 2",
            "10 MB extra",
            "1e9",
            "0x10",
            "1,000",
            "1_000",
            "+1",
            "1k2",
            "1 kk",
            "10  mb",
            "1kib b",
            // negatives
            "-1",
            "-1k",
            "- 1",
            // overflow: representable as a number, not as a size
            "18446744073709551616",
            "16e",
            "17e",
            "9999999999999999999999999999999999999999",
            // Not every word a person types is ASCII, and a size that is not
            // one has to come back as a refusal rather than as a panic.
            "1€",
            "€",
            "1 €",
            "1k€",
            "١٢٣",
            "1２",
            "1kв",
            "🙂",
            "1 MiB🙂",
        ];
        for text in refused {
            let outcome = parse_size(text);
            assert!(outcome.is_err(), "{text:?} was read as {outcome:?}");
            assert!(
                outcome.unwrap_err().starts_with("not a size: "),
                "{text:?} was refused in the wrong words"
            );
        }
        // The line is where u64 stops, not where the digits do.
        assert_eq!(parse_size("15e"), Ok(15 * (1_u64 << 60)));
        assert_eq!(parse_size("1.5e"), Ok(15 * (1_u64 << 60) / 10));
    }

    #[test]
    fn nothing_a_person_can_type_makes_the_parser_fall_over() {
        // A flag value is whatever came off the command line. Refusing is an
        // answer; panicking is a crash on a program that deletes files.
        let alphabet: Vec<char> = "0123456789.,-+ kmgtpeKMGTPEbiBIxе€🙂\t\n\u{0}١２"
            .chars()
            .collect();
        let mut seed = 0xDEAD_BEEF_CAFE_F00D_u64;
        for _ in 0..20000 {
            let mut word = String::new();
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            for _ in 0..(seed % 12) {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                word.push(alphabet[(seed >> 33) as usize % alphabet.len()]);
            }
            // Whatever it decides, it decides without unwinding.
            let _ = parse_size(&word);
        }
    }

    #[test]
    fn a_size_is_shown_the_way_it_would_be_typed() {
        let cases: &[(u64, &str)] = &[
            (0, "0"),
            (1, "1"),
            (1023, "1023"),
            (1024, "1 KiB"),
            (1536, "1.5 KiB"),
            (10 * M, "10 MiB"),
            (M, "1 MiB"),
            (100 * K, "100 KiB"),
            (5 * G, "5 GiB"),
            (G + G / 2, "1.5 GiB"),
            // Nothing divides these, so they stay bytes rather than lie.
            (1_500_000, "1500000"),
            (10 * M + 1, "10485761"),
            (u64::MAX, "18446744073709551615"),
        ];
        for &(bytes, shown) in cases {
            assert_eq!(exact_size(bytes), shown, "{bytes}");
        }
    }

    #[test]
    fn every_size_the_tool_would_show_as_a_setting_parses_back_to_itself() {
        // The display round-trip law. A form or a help line that showed a
        // rounded size would be promising a scan the tool is not about to run.
        let mut values: Vec<u64> = vec![
            0,
            1,
            1023,
            1024,
            1536,
            crate::consts::DEFAULT_MIN_SIZE,
            u64::MAX,
        ];
        // A spread that lands on and beside every rung of the ladder, plus
        // numbers that sit on none of them.
        let mut unit = 1_u64;
        for _ in 0..6 {
            unit = unit.wrapping_mul(1024);
            for step in [1_u64, 2, 3, 10, 100, 999, 1023] {
                for nudge in [0_i64, -1, 1, 512] {
                    let value = unit
                        .checked_mul(step)
                        .and_then(|n| n.checked_add_signed(nudge));
                    values.extend(value);
                    values.extend(value.map(|n| n / 2));
                    values.extend(value.map(|n| n / 10));
                }
            }
        }
        // ...and a deterministic scatter of numbers nobody chose.
        let mut seed = 0x243F_6A88_85A3_08D3_u64;
        for _ in 0..2000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            values.push(seed);
            values.push(seed >> 20);
            values.push(seed >> 40);
        }
        for bytes in values {
            let shown = exact_size(bytes);
            assert_eq!(
                parse_size(&shown),
                Ok(bytes),
                "{bytes} was shown as {shown:?} and read back as something else"
            );
        }
    }

    #[test]
    fn one_ladder_is_read_and_written() {
        // The bond that stops the two sides drifting. A rung added to the
        // display and not to the grammar would be a size the tool shows and
        // cannot be told; a rung added to the grammar and not to the display
        // would be a size nobody is ever shown.
        for (rung, unit) in LADDER.iter().enumerate() {
            let bytes = 1_u64 << (10 * (rung + 1));
            assert_eq!(parse_size(&format!("1 {unit}")), Ok(bytes), "{unit}");
            assert_eq!(exact_size(bytes), format!("1 {unit}"), "{unit}");
            assert!(human_size(bytes).ends_with(unit), "{unit}");
        }
        assert_eq!(parse_size("1 B"), Ok(1));
        assert_eq!(human_size(1), "1 B");
    }

    /// The first byte count `human_size` rounds up to `16.0 EiB` — sixteen
    /// exbibytes being 2⁶⁴, one byte more than there is, so it is the one
    /// string the browser can show that the grammar is right to refuse.
    ///
    /// Searched for rather than written down: a change to the rounding should
    /// move this boundary, not break the arithmetic that names it. The search
    /// stands on `human_size` being non-decreasing, which the assertions below
    /// check at the boundary itself.
    fn rounds_past_u64() -> u64 {
        let (mut low, mut high) = (1_u64 << 63, u64::MAX);
        while low < high {
            let middle = low + (high - low) / 2;
            if parse_size(&human_size(middle)).is_err() {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        low
    }

    #[test]
    fn what_the_browser_shows_is_at_least_readable_back() {
        // `human_size` rounds, so it makes no promise about the number — that
        // law is `exact_size`'s. It does have to stay inside the grammar,
        // which is what fails the moment somebody spells a unit a new way.
        //
        // The whole u64 range, boundary included. Above `ROUNDS_PAST_U64` the
        // only string it can produce is one the grammar is right to refuse, and
        // asserting that is worth more than shifting the sweep to miss it.
        let cliff = rounds_past_u64();
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut past = 0;
        for _ in 0..5000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            for bytes in [seed, seed >> 4, seed >> 20] {
                let shown = human_size(bytes);
                if bytes >= cliff {
                    past += 1;
                    assert_eq!(shown, "16.0 EiB", "{bytes}");
                    assert!(parse_size(&shown).is_err(), "{bytes} rounds to 2^64");
                    continue;
                }
                assert!(
                    parse_size(&shown).is_ok(),
                    "{bytes} was shown as {shown:?}, which the grammar cannot read"
                );
            }
        }
        assert!(past > 0, "the sweep never reached the boundary");
    }

    #[test]
    fn the_one_size_the_browser_can_show_that_the_grammar_refuses() {
        // The boundary itself, from both sides, so a change to the rounding or
        // to the ladder has to come past this test rather than around it.
        let cliff = rounds_past_u64();
        assert_eq!(human_size(cliff), "16.0 EiB");
        assert!(parse_size("16.0 EiB").is_err());
        assert!(parse_size(&human_size(cliff - 1)).is_ok());
        assert_ne!(human_size(cliff - 1), "16.0 EiB");
        // It is the very top of the range and nothing else: below the cliff
        // every rung reads back, and above it there is nothing but 16.0 EiB.
        assert!(
            cliff > 15 * (1_u64 << 60),
            "the cliff is not in the top rung"
        );
        assert_eq!(human_size(u64::MAX), "16.0 EiB");
        // ...and `exact_size`, whose law this actually is, round-trips at both
        // sides of it and at the very end.
        for bytes in [cliff - 1, cliff, u64::MAX] {
            assert_eq!(parse_size(&exact_size(bytes)), Ok(bytes), "{bytes}");
        }
    }

    #[test]
    fn the_rounded_humaniser_still_reads_the_way_the_oracle_writes() {
        // dedup.py's `human_size`, with the labels made honest: it divided by
        // 1024 and called the result KB throughout.
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(100 * K), "100 KiB");
        assert_eq!(human_size(M), "1.0 MiB");
        assert_eq!(human_size(5 * G), "5.0 GiB");
        // The oracle stops dividing after PB and prints what is left as EB,
        // which is 1024 times the truth. This does not.
        assert_eq!(human_size(u64::MAX), "16.0 EiB");
        assert_eq!(human_size(1 << 60), "1.0 EiB");
    }

    #[test]
    fn counts_are_grouped_in_threes() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1000), "1,000");
        assert_eq!(human_count(1234567), "1,234,567");
        assert_eq!(human_count(u64::MAX), "18,446,744,073,709,551,615");
    }
}
