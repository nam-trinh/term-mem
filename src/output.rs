//! Output conventions from docs/cli.md: human by default, machine on request,
//! pipe detection, and exit codes that carry meaning.

use crate::db::queries::Exchange;
use crate::search::{Hit, HL_CLOSE, HL_OPEN};

/// Bold yellow, and its reset. Every `HL_ON` in a rendered string has a
/// matching `HL_OFF`, including on a truncated one.
const HL_ON: &str = "\x1b[1;33m";
const HL_OFF: &str = "\x1b[0m";
use std::io::IsTerminal;

/// docs/cli.md: `0` found, `1` nothing found, `2` error.
pub const EXIT_OK: i32 = 0;
pub const EXIT_EMPTY: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

pub fn is_tty() -> bool {
    std::io::stdout().is_terminal()
}

fn dim(s: &str) -> String {
    if is_tty() {
        format!("\x1b[2m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn bold(s: &str) -> String {
    if is_tty() {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// `YYYY-MM-DD` from unix ms, UTC.
pub fn fmt_date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn fmt_datetime(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let tod = secs.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}:{:02}Z",
        fmt_date(ms),
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Year, month, day for a unix-ms instant, UTC.
pub fn civil_from_ms(ms: i64) -> (i64, i64, i64) {
    civil_from_days(ms.div_euclid(86_400_000))
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Collapse a path back to `~/…` — the archive stores absolute paths, but a
/// result list of them is unreadable.
pub fn tilde(p: &str) -> String {
    let Ok(home) = std::env::var("HOME") else {
        return p.to_string();
    };
    let home = home.trim_end_matches('/');
    if home.is_empty() || !p.starts_with(home) {
        return p.to_string();
    }
    // Only at a path boundary: with HOME=/home/dev/a, the path
    // /home/dev/a[1]/sub is not inside it and must not render as `~[1]/sub`.
    match &p[home.len()..] {
        "" => "~".to_string(),
        rest if rest.starts_with('/') => format!("~{rest}"),
        _ => p.to_string(),
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// The scannable list from docs/cli.md: the prompt as the title, the highest
/// signal line of the response beneath it. Snippets, not transcripts.
pub fn print_list(rows: &[Exchange]) {
    let tty = is_tty();
    for (i, ex) in rows.iter().enumerate() {
        if tty {
            println!(
                "{:>2}.  {}   {}   {}",
                i + 1,
                bold(&ex.id),
                fmt_date(ex.ts),
                dim(&tilde(&ex.cwd))
            );
            println!("    \"{}\"", one_line(&ex.prompt, 92));
            if let Some(c) = ex.commands.first() {
                println!("    → {}", one_line(c, 92));
            } else if !ex.response.trim().is_empty() {
                println!("    {}", dim(&one_line(&ex.response, 92)));
            }
            println!();
        } else {
            // One record per line when stdout is not a terminal.
            println!(
                "{}\t{}\t{}\t{}",
                ex.id,
                fmt_date(ex.ts),
                tilde(&ex.cwd),
                one_line(&ex.prompt, 120)
            );
        }
    }
}

pub fn print_full(ex: &Exchange) {
    println!("{}  {}", bold(&ex.id), fmt_datetime(ex.ts));
    println!("{}", dim(&format!("cwd     {}", tilde(&ex.cwd))));
    if let Some(r) = &ex.repo {
        println!(
            "{}",
            dim(&format!(
                "repo    {}{}",
                r,
                ex.git_branch
                    .as_deref()
                    .map(|b| format!(" ({b})"))
                    .unwrap_or_default()
            ))
        );
    }
    println!(
        "{}",
        dim(&format!(
            "via     {}{}",
            ex.assistant,
            ex.model
                .as_deref()
                .map(|m| format!(" / {m}"))
                .unwrap_or_default()
        ))
    );
    if ex.redacted {
        println!("{}", dim("note    contains redacted content"));
    }
    println!("\n{}\n\n{}", bold("prompt"), ex.prompt.trim());
    if !ex.response.trim().is_empty() {
        println!("\n{}\n\n{}", bold("response"), ex.response.trim());
    }
    if !ex.commands.is_empty() {
        println!("\n{}\n", bold("commands"));
        for c in &ex.commands {
            println!("  {c}");
        }
    }
    if !ex.files.is_empty() {
        println!("\n{}\n", bold("files touched"));
        for f in &ex.files {
            println!("  {}", tilde(f));
        }
    }
}

pub fn print_json<T: serde::Serialize>(rows: &[T]) -> anyhow::Result<()> {
    for ex in rows {
        println!("{}", serde_json::to_string(ex)?);
    }
    Ok(())
}

/// Search results. The same scannable shape as `print_list`, plus the matched
/// region — which is the whole point of a result list you can skim.
///
/// docs/tech-stack.md: a snippet is "expanded to a whole line or fenced block
/// so a command is never shown truncated". A truncated command line is worse
/// than no command line, because it looks copyable and isn't.
pub fn print_hits(hits: &[Hit]) {
    let tty = is_tty();
    for (i, h) in hits.iter().enumerate() {
        let ex = &h.exchange;
        let matched = matched_terms(&h.snippet);
        if tty {
            println!(
                "{:>2}.  {}   {}   {}",
                i + 1,
                bold(&ex.id),
                fmt_date(ex.ts),
                dim(&tilde(&ex.cwd))
            );
            println!("    \"{}\"", one_line(&ex.prompt, 92));
            match ex.commands.iter().find(|c| hits_any(c, &matched)) {
                Some(cmd) => println!("    → {}", highlight(cmd, &matched, tty)),
                None => println!("    {}", render_snippet(&h.snippet, tty)),
            }
            println!();
        } else {
            println!(
                "{}\t{}\t{}\t{}",
                ex.id,
                fmt_date(ex.ts),
                tilde(&ex.cwd),
                render_snippet(&h.snippet, tty)
            );
        }
    }
}

/// The terms FTS5 actually matched, lifted back out of its own markers. Cheaper
/// and more honest than re-deriving them from the query: stemming means the
/// text that matched is often not the text that was typed.
fn matched_terms(snippet: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = snippet;
    while let Some(o) = rest.find(HL_OPEN) {
        let after = &rest[o + HL_OPEN.len_utf8()..];
        let Some(c) = after.find(HL_CLOSE) else { break };
        let term = after[..c].to_lowercase();
        if !term.is_empty() && !out.contains(&term) {
            out.push(term);
        }
        rest = &after[c + HL_CLOSE.len_utf8()..];
    }
    out
}

fn hits_any(text: &str, terms: &[String]) -> bool {
    let lower = text.to_lowercase();
    terms.iter().any(|t| lower.contains(t.as_str()))
}

/// How many bytes of `hay` a case-insensitive match of `term` consumes, if it
/// matches at the start.
///
/// Written this way because the obvious version is a latent panic: lowercasing
/// the haystack and indexing it with offsets taken from the original assumes
/// the two have the same byte length, and `İ` (U+0130) lowercases to two chars.
/// A command line containing one used to abort the process with exit 101.
fn match_len(hay: &str, term: &str) -> Option<usize> {
    let mut lowered = String::new();
    let mut consumed = 0;
    for ch in hay.chars() {
        lowered.extend(ch.to_lowercase());
        consumed += ch.len_utf8();
        if lowered == term {
            return Some(consumed);
        }
        if !term.starts_with(&lowered) {
            return None;
        }
    }
    None
}

fn highlight(text: &str, terms: &[String], tty: bool) -> String {
    let flat = one_line(text, 200);
    if !tty {
        return flat;
    }
    let mut out = String::with_capacity(flat.len());
    let mut i = 0;
    while i < flat.len() {
        let longest = terms.iter().filter_map(|t| match_len(&flat[i..], t)).max();
        match longest {
            Some(n) => {
                out.push_str(HL_ON);
                out.push_str(&flat[i..i + n]);
                out.push_str(HL_OFF);
                i += n;
            }
            None => {
                let ch = flat[i..].chars().next().expect("i is a char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Swap FTS5's markers for terminal escapes, or drop them on a pipe — the
/// markers are control characters and must never reach a file.
///
/// Truncation counts *visible* characters and happens in the same pass, because
/// cutting the string first can cut between an opening marker and its closing
/// one — which leaves the escape unbalanced and the user's terminal bold yellow
/// after the process exits.
fn render_snippet(snippet: &str, tty: bool) -> String {
    const MAX: usize = 200;
    let flat = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(flat.len());
    let mut visible = 0;
    let mut open = false;
    let mut truncated = false;

    for ch in flat.chars() {
        match ch {
            HL_OPEN => {
                if tty {
                    out.push_str(HL_ON);
                    open = true;
                }
            }
            HL_CLOSE => {
                if tty && open {
                    out.push_str(HL_OFF);
                    open = false;
                }
            }
            _ => {
                if visible == MAX {
                    truncated = true;
                    break;
                }
                out.push(ch);
                visible += 1;
            }
        }
    }
    if open {
        out.push_str(HL_OFF);
    }
    if truncated {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_terms_come_back_out_of_the_markers() {
        let s = format!("a {HL_OPEN}Concat{HL_CLOSE} b {HL_OPEN}ffmpeg{HL_CLOSE}");
        assert_eq!(matched_terms(&s), vec!["concat", "ffmpeg"]);
    }

    /// The markers are U+0001 and U+0002. A pipe must receive neither them nor
    /// an escape sequence.
    #[test]
    fn a_pipe_gets_no_control_characters() {
        let s = format!("x {HL_OPEN}y{HL_CLOSE} z");
        let out = render_snippet(&s, false);
        assert_eq!(out, "x y z");
        assert!(!out.contains('\x1b'));
    }

    /// A command line containing `İ` (U+0130) used to abort the process with
    /// "start byte index N is not a char boundary" and exit 101, which is not
    /// one of the three exit codes docs/cli.md promises. U+212A took the
    /// out-of-bounds path instead.
    #[test]
    fn highlighting_does_not_panic_when_lowercasing_changes_byte_length() {
        for text in ["ffmpeg -İ concat", "ffmpeg -\u{212a} concat", "İIİ ffmpeg"] {
            let out = highlight(text, &["ffmpeg".to_string()], true);
            assert!(out.contains("ffmpeg"), "{text} -> {out}");
        }
        // And the term itself may be the awkward one.
        let out = highlight("İstanbul notes", &["i\u{307}stanbul".to_string()], true);
        assert!(out.contains("İstanbul"), "{out}");
    }

    /// Truncation used to happen before the markers became escapes, so a cut
    /// between an opening marker and its close emitted `\x1b[1;33m` with no
    /// reset and left the terminal bold yellow after tmem exited.
    #[test]
    fn a_truncated_snippet_still_closes_its_escape() {
        // The cut has to land *between* the two markers, so the padding is
        // swept rather than guessed: at 200 visible characters the interesting
        // widths are around half that in "x " pairs.
        for pad in 80..210 {
            let s = format!("{}{HL_OPEN}match{HL_CLOSE} tail", "x ".repeat(pad));
            let out = render_snippet(&s, true);
            assert_eq!(
                out.matches(HL_ON).count(),
                out.matches(HL_OFF).count(),
                "unbalanced at pad {pad}: {out:?}"
            );
            assert!(
                !out.ends_with(HL_ON),
                "opened a highlight with nothing in it at pad {pad}"
            );
        }
    }

    #[test]
    fn highlighting_is_case_insensitive_and_leaves_text_intact() {
        let out = highlight("FFmpeg -f concat", &["ffmpeg".to_string()], true);
        assert!(out.contains("FFmpeg"), "original case is preserved: {out}");
        assert!(out.contains('\x1b'));
        assert_eq!(
            highlight("FFmpeg -f concat", &["ffmpeg".into()], false),
            "FFmpeg -f concat"
        );
    }
}
