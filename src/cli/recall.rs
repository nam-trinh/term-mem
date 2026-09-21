//! Automatic recall on `UserPromptSubmit` — **off by default**.
//!
//! docs/plan.md, Phase 4: "Optional `UserPromptSubmit` automatic recall: off by
//! default, capped at 3 exchanges and ~1500 tokens, and always visibly
//! attributed. Memory injected invisibly is indistinguishable from the model
//! hallucinating confidently."
//!
//! Three things follow from that sentence and each is load-bearing:
//!
//! * **Off by default is off, not idle.** `tmem recall --enable` is what
//!   registers the hook; until then there is no `UserPromptSubmit` entry in
//!   settings.json at all. A registered hook that reads a flag and exits still
//!   costs a process spawn on every prompt the user types, and still has to be
//!   trusted to read the flag correctly.
//! * **Capped, and the cap is enforced where the text is built**, in
//!   [`crate::cli::render`], not by hoping the query returns little.
//! * **Visibly attributed, twice.** The block says where it came from, and a
//!   one-line summary naming the ids goes to stderr so it surfaces in the
//!   user's own terminal rather than only inside the model's context.

use crate::cli::render::{self, Record, CHARS_PER_TOKEN};
use crate::db;
use crate::db::queries::Filter;
use crate::output::{fmt_date, tilde, EXIT_EMPTY, EXIT_OK};
use crate::paths;
use crate::search::{self, Hit};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;

pub const HOOK_EVENT: &str = "UserPromptSubmit";
pub const HOOK_COMMAND: &str = "tmem recall --hook";

/// The caps from docs/plan.md, plus the relevance floor.
///
/// docs/plan.md asks for injection "if anything clears a relevance floor", and
/// the obvious floor — a minimum BM25 score — **does not work and was removed
/// after being built**. BM25's scale is not comparable across archives: its IDF
/// term collapses when every document contains the word, so on a three-row
/// archive a perfect match scores 5e-06 and on a 100k-row one the same match
/// scores double digits. A fixed number is therefore "always on" or "always
/// off" depending on how much history the user has, which is the worst possible
/// behaviour for a feature that spends the user's context window.
///
/// What replaces it is **term coverage**: how many of the prompt's own content
/// words are physically present in the exchange. That is archive-size
/// independent, it is what `tmem recall <query>` prints, and it is a sentence a
/// user can check. BM25 still orders the candidates; it just no longer decides
/// whether any of them are good enough.
///
/// `min_terms = 0` means automatic — two words, or a quarter of the prompt's
/// content words, whichever is more, and never more than four.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    pub max_exchanges: usize,
    pub max_tokens: usize,
    pub min_terms: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: false,
            max_exchanges: 3,
            max_tokens: 1500,
            min_terms: 0,
        }
    }
}

impl Config {
    /// Read the settings, or fail saying why.
    ///
    /// A broken file is an error for the same reason a broken `redact.toml` is:
    /// a user who edited it believes the number they wrote is in force, and
    /// silently falling back to ours makes them wrong.
    ///
    /// The difference — and it was got wrong first time — is **what the error
    /// should cost**. A redaction rule that will not compile has to stop
    /// capture, because capturing unredacted is worse. A recall config that
    /// will not parse should stop *recall*, and nothing else. Making it fatal
    /// everywhere took out `tmem status` halfway through its output and, worse,
    /// took out `tmem recall --disable` — so the one command that could fix the
    /// file was the one command that could not run. Callers that must keep
    /// working use [`Config::load_or_default`].
    pub fn load() -> Result<Config> {
        let p = paths::recall_config()?;
        let Ok(text) = std::fs::read_to_string(&p) else {
            return Ok(Config::default());
        };
        toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))
    }

    /// The settings, or the defaults plus the reason they are being used.
    ///
    /// The defaults are **off**, so a file nobody can parse fails in the
    /// direction that injects nothing rather than the direction that injects
    /// something the user did not ask for.
    pub fn load_or_default() -> (Config, Option<String>) {
        match Config::load() {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("{e:#}"))),
        }
    }

    fn save(&self) -> Result<()> {
        let p = paths::recall_config()?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// The `UserPromptSubmit` payload. `prompt` is the field that makes this hook
/// different from `Stop`: it carries the text, so recall never has to go and
/// read the transcript.
#[derive(Debug, Deserialize, Default)]
struct PromptPayload {
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    session_id: Option<String>,
}

// The payload also carries `cwd`, and it is deliberately unused. Preferring
// exchanges from the directory the user is standing in is plausible and
// unmeasured, and a relevance heuristic nobody has checked is exactly the kind
// of thing that makes injected context feel arbitrary. `--in` is the explicit
// lever and it stays explicit.

// ------------------------------------------------------------------ verbs

pub fn enable() -> Result<i32> {
    let mut cfg = Config::load()?;
    cfg.enabled = true;
    // The hook first, the config second. The other order leaves `enabled =
    // true` on disk after an `add_hook` that failed and exited 2 — a permanent
    // INCONSISTENT state produced by a command the user watched fail.
    let state = crate::cli::init::add_hook(HOOK_EVENT, HOOK_COMMAND)?;
    cfg.save()?;
    match state {
        crate::cli::init::HookState::Added(p) => {
            println!("automatic recall ON — hook registered in {}", tilde(&p))
        }
        crate::cli::init::HookState::AlreadyPresent(p) => {
            println!("automatic recall ON — hook already in {}", tilde(&p))
        }
    }
    println!(
        "  at most {} exchange(s) and ~{} tokens are prepended to a prompt, and only when\n  \
         enough of what you typed actually appears in them. Every block says it came from\n  \
         term-mem and names the id of each exchange, so nothing reaches the model that you\n  \
         cannot go and read.",
        cfg.max_exchanges, cfg.max_tokens
    );
    println!(
        "  settings:   {}",
        tilde(&paths::recall_config()?.to_string_lossy())
    );
    println!("  preview:    tmem recall <words from a prompt>");
    println!("  off again:  tmem recall --disable");
    Ok(EXIT_OK)
}

pub fn disable() -> Result<i32> {
    // `load_or_default`, not `load`: this is the command that fixes a broken
    // config file, so it must not be stopped by one. The whole file is
    // overwritten with a valid one below, which is also the repair.
    let (mut cfg, broken) = Config::load_or_default();
    if let Some(why) = broken {
        eprintln!("tmem: {why}\n  (rewriting it with the defaults, which are off)");
    }
    cfg.enabled = false;
    cfg.save()?;
    let n = crate::cli::init::remove_hook(HOOK_EVENT, HOOK_COMMAND)?;
    println!(
        "automatic recall OFF{}",
        if n > 0 {
            " — hook removed from Claude Code's settings"
        } else {
            ""
        }
    );
    println!("  capture is unaffected; `tmem <query>` still searches everything.");
    Ok(EXIT_OK)
}

pub fn status() -> Result<i32> {
    let (cfg, broken) = Config::load_or_default();
    if let Some(why) = broken {
        println!("  recall      UNREADABLE SETTINGS — nothing is injected");
        println!("  error       {why}");
        println!("  fix         tmem recall --disable, then --enable");
        return Ok(crate::output::EXIT_ERROR);
    }
    let hooked = crate::cli::init::hook_registered(HOOK_EVENT, HOOK_COMMAND);
    println!(
        "  recall      {}",
        if cfg.enabled && hooked {
            "ON"
        } else if cfg.enabled || hooked {
            "INCONSISTENT — see below"
        } else {
            "off (the default)"
        }
    );
    println!(
        "  caps        {} exchange(s), ~{} tokens, {}",
        cfg.max_exchanges,
        cfg.max_tokens,
        describe_floor(&cfg)
    );
    if cfg.enabled != hooked {
        // Never paper over it: one of the two halves was changed by hand and
        // the user's belief about what reaches the model is wrong either way.
        println!(
            "  warning     config says enabled={}, but the {HOOK_EVENT} hook is {}.\n              \
             Run `tmem recall --enable` or `--disable` to make them agree.",
            cfg.enabled,
            if hooked { "registered" } else { "absent" }
        );
    }
    println!("  turn on     tmem recall --enable");
    println!("  preview     tmem recall <words from a prompt>");
    Ok(EXIT_OK)
}

/// `tmem recall <words>` — show exactly what would be injected for a prompt,
/// and why each exchange was chosen. This is the user-facing half of the Exit
/// criterion: the recall path is inspectable without a live session.
pub fn preview(terms: &[String]) -> Result<i32> {
    let (cfg, broken) = Config::load_or_default();
    if let Some(why) = broken {
        eprintln!("tmem: {why}\n  (previewing with the defaults instead)");
    }
    let text = terms.join(" ");
    let chosen = select(&text, None, &cfg)?;
    if chosen.is_empty() {
        eprintln!(
            "tmem recall: nothing clears the bar for that prompt — no context would be \
             injected.\n  ({}; `tmem {}` shows what search finds regardless)",
            describe_floor(&cfg),
            text
        );
        return Ok(EXIT_EMPTY);
    }
    eprintln!(
        "tmem recall: {} exchange(s) would be prepended",
        chosen.len()
    );
    for c in &chosen {
        eprintln!("  {}", c.explain());
    }
    if !cfg.enabled {
        eprintln!("  (automatic recall is off; this is a preview — `tmem recall --enable`)");
    }
    let text = block(&chosen, &cfg);
    if text.is_empty() {
        eprintln!(
            "tmem recall: max_tokens = {} is too small for a context block",
            cfg.max_tokens
        );
        return Ok(EXIT_EMPTY);
    }
    print!("{text}");
    Ok(EXIT_OK)
}

/// The hook itself.
///
/// Its contract is narrower than it looks: **it must never fail the turn.** A
/// prompt the user typed is not something to lose over a missing database or a
/// malformed payload, so every path here ends in exit 0 with the reason on
/// stderr. The worst outcome is that no context is injected, which is exactly
/// what happens when recall is off anyway.
pub fn hook() -> Result<i32> {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        eprintln!("tmem recall: could not read the hook payload; nothing injected");
        return Ok(EXIT_OK);
    }
    let payload: PromptPayload = match serde_json::from_str(&buf) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tmem recall: unreadable {HOOK_EVENT} payload ({e}); nothing injected");
            return Ok(EXIT_OK);
        }
    };
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tmem recall: {e:#}; nothing injected");
            return Ok(EXIT_OK);
        }
    };
    if !cfg.enabled {
        return Ok(EXIT_OK);
    }
    if payload.prompt.trim().is_empty() {
        return Ok(EXIT_OK);
    }
    let chosen = match select(&payload.prompt, payload.session_id.as_deref(), &cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tmem recall: {e:#}; nothing injected");
            return Ok(EXIT_OK);
        }
    };
    if chosen.is_empty() {
        return Ok(EXIT_OK);
    }
    let text = block(&chosen, &cfg);
    if text.is_empty() {
        // `max_tokens` set too low to hold the block's own header. Inject
        // nothing rather than a wrapper with no memory in it, and say why —
        // the user configured this and is the only one who can undo it.
        eprintln!(
            "tmem recall: max_tokens = {} is too small for a context block; nothing injected",
            cfg.max_tokens
        );
        return Ok(EXIT_OK);
    }

    // The visible half. stderr, so it reaches the user's terminal rather than
    // only the model's context — the block itself carries the attribution on
    // the other side.
    eprintln!(
        "tmem: recalled {} past exchange(s) — {}",
        chosen.len(),
        chosen
            .iter()
            .map(|c| format!("tmem show {}", c.hit.exchange.id))
            .collect::<Vec<_>>()
            .join(" · ")
    );

    println!(
        "{}",
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": HOOK_EVENT,
                "additionalContext": text,
            }
        })
    );
    Ok(EXIT_OK)
}

// --------------------------------------------------------------- selection

pub struct Chosen {
    pub hit: Hit,
    pub matched: Vec<String>,
    pub coverage: usize,
    pub of_terms: usize,
}

impl Chosen {
    pub fn explain(&self) -> String {
        format!(
            "{}  {}  {}  score {:.2}, {}/{} query term(s) present ({})",
            self.hit.exchange.id,
            fmt_date(self.hit.exchange.ts),
            tilde(&self.hit.exchange.cwd),
            self.hit.score,
            self.coverage,
            self.of_terms,
            if self.matched.is_empty() {
                "—".to_string()
            } else {
                self.matched.join(", ")
            }
        )
    }
}

/// Words too common to carry any signal. Short by design: FTS5's own stopword
/// handling is none at all, and this list exists to stop a 300-word prompt
/// becoming a 300-term OR query, not to do linguistics.
const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "you",
    "are",
    "was",
    "were",
    "have",
    "has",
    "had",
    "can",
    "could",
    "would",
    "should",
    "what",
    "when",
    "where",
    "which",
    "how",
    "why",
    "does",
    "did",
    "not",
    "but",
    "from",
    "into",
    "our",
    "out",
    "its",
    "his",
    "her",
    "them",
    "they",
    "then",
    "than",
    "there",
    "here",
    "about",
    "just",
    "like",
    "some",
    "any",
    "all",
    "get",
    "got",
    "let",
    "make",
    "made",
    "use",
    "using",
    "used",
    "need",
    "want",
    "please",
    "help",
    "now",
    "new",
    "one",
    "two",
    "also",
    "very",
    "more",
    "most",
    "who",
    "will",
    "without",
    "within",
    "been",
    "being",
    "because",
    "before",
    "after",
    "again",
    "still",
    "something",
    "anything",
    "everything",
    "know",
    "think",
    "tell",
    "show",
    "give",
    "look",
];

/// A prompt is not a query. Turning one into terms is a lossy step and it is
/// where the "every prompt recalls something" failure mode begins, so it is
/// deliberately aggressive: long-ish words only, no stopwords, capped in count.
pub fn terms_of(prompt: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in prompt.split(|c: char| !c.is_alphanumeric()) {
        let t = raw.to_lowercase();
        if t.chars().count() < 3 || STOPWORDS.contains(&t.as_str()) {
            continue;
        }
        if !out.contains(&t) {
            out.push(t);
        }
        if out.len() == 24 {
            break;
        }
    }
    out
}

/// Does the row actually contain the term, anywhere? BM25's score says "this
/// row ranks highest"; it never says "this row is relevant", and on a small
/// archive the highest-ranked row for an unrelated prompt is still some row.
/// Coverage is the cheap sanity check on top: at least two of the prompt's own
/// words have to be physically present.
fn coverage(hit: &Hit, terms: &[String]) -> (usize, Vec<String>) {
    let ex = &hit.exchange;
    let hay = format!(
        "{} {} {}",
        ex.prompt.to_lowercase(),
        ex.response.to_lowercase(),
        ex.commands.join(" ").to_lowercase()
    );
    let found: Vec<String> = terms
        .iter()
        .filter(|t| hay.contains(t.as_str()))
        .cloned()
        .collect();
    (found.len(), found)
}

/// How many of the prompt's content words an exchange has to contain.
///
/// Two is the floor because one shared word is a coincidence — every archive
/// with a "test" in it matches every prompt with a "test" in it. The quarter
/// rule scales that up for long prompts without ever demanding so much that a
/// paraphrase fails, and four is the ceiling because a user who writes a
/// paragraph is not going to reuse a quarter of it verbatim.
pub fn required_terms(cfg: &Config, n_terms: usize) -> usize {
    if cfg.min_terms > 0 {
        return cfg.min_terms.min(n_terms.max(1));
    }
    if n_terms <= 1 {
        return 1;
    }
    n_terms.div_ceil(4).clamp(2, 4).min(n_terms)
}

pub fn describe_floor(cfg: &Config) -> String {
    if cfg.min_terms > 0 {
        format!("at least {} query term(s) present", cfg.min_terms)
    } else {
        "at least two query terms present (a quarter of a long prompt, up to four)".to_string()
    }
}

/// Pick what, if anything, is worth injecting.
///
/// Three gates, all of which must pass: term coverage, not being the asking
/// session, and the hard cap on how many. Exchanges from the session doing the
/// asking are dropped outright — they are already in that session's context,
/// and paying tokens to tell a model what it just said is the cheapest way to
/// make this feature feel broken.
pub fn select(prompt: &str, session_id: Option<&str>, cfg: &Config) -> Result<Vec<Chosen>> {
    let terms = terms_of(prompt);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let conn = db::open_readonly(&paths::db_path()?)?;
    let filter = Filter {
        // A pool wider than the cap, because the filters below throw rows away.
        limit: Some((cfg.max_exchanges * 4).max(10)),
        ..Default::default()
    };
    let hits = search::search(&conn, &terms, &filter)?;

    let need = required_terms(cfg, terms.len());
    let mut out = Vec::new();
    for hit in hits {
        if out.len() >= cfg.max_exchanges {
            break;
        }
        if session_id.is_some() && Some(hit.exchange.session_id.as_str()) == session_id {
            continue;
        }
        let (n, matched) = coverage(&hit, &terms);
        if n < need {
            continue;
        }
        out.push(Chosen {
            hit,
            matched,
            coverage: n,
            of_terms: terms.len(),
        });
    }
    Ok(out)
}

fn block(chosen: &[Chosen], cfg: &Config) -> String {
    let records: Vec<Record> = chosen
        .iter()
        .map(|c| {
            let ex = &c.hit.exchange;
            Record {
                id: ex.id.clone(),
                ts: ex.ts,
                cwd: ex.cwd.clone(),
                repo: ex.repo.clone(),
                git_branch: ex.git_branch.clone(),
                assistant: Some(ex.assistant.clone()),
                prompt: ex.prompt.clone(),
                response: ex.response.clone(),
                commands: ex.commands.clone(),
                snippet: None,
                redacted: ex.redacted,
            }
        })
        .collect();
    render::prompt_block(&records, Some(cfg.max_tokens * CHARS_PER_TOKEN))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_becomes_a_short_list_of_content_words() {
        let t = terms_of("How do I concat mp4 files with ffmpeg, without re-encoding?");
        assert!(t.contains(&"concat".to_string()));
        assert!(t.contains(&"ffmpeg".to_string()));
        assert!(t.contains(&"encoding".to_string()));
        for stop in ["how", "the", "with", "without"] {
            assert!(!t.contains(&stop.to_string()), "{stop} survived: {t:?}");
        }
        assert!(
            !t.contains(&"do".to_string()),
            "two-letter words are dropped"
        );
    }

    /// A 2,000-word paste must not become a 2,000-term OR query on the turn
    /// boundary.
    #[test]
    fn term_extraction_is_capped() {
        let prompt = (0..500)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(terms_of(&prompt).len(), 24);
    }

    #[test]
    fn terms_are_deduplicated_and_an_empty_prompt_yields_nothing() {
        assert_eq!(terms_of("ffmpeg ffmpeg FFMPEG"), vec!["ffmpeg"]);
        assert!(terms_of("").is_empty());
        assert!(terms_of("a of  !! ???").is_empty());
    }

    /// Off by default is the whole point, and a config file that has never
    /// been written must not read as "on".
    #[test]
    fn the_default_config_is_off_with_the_caps_the_plan_states() {
        let c = Config::default();
        assert!(!c.enabled);
        assert_eq!(c.max_exchanges, 3);
        assert_eq!(c.max_tokens, 1500);
        assert_eq!(c.min_terms, 0, "automatic");
    }

    /// The gate scales with the prompt, and never asks for more words than the
    /// prompt has.
    #[test]
    fn the_coverage_floor_scales_and_is_never_unsatisfiable() {
        let c = Config::default();
        assert_eq!(required_terms(&c, 0), 1);
        assert_eq!(required_terms(&c, 1), 1);
        assert_eq!(required_terms(&c, 2), 2);
        assert_eq!(required_terms(&c, 6), 2);
        assert_eq!(required_terms(&c, 12), 3);
        assert_eq!(required_terms(&c, 24), 4);
        // An explicit setting is honoured, but still cannot exceed the prompt.
        let c = Config {
            min_terms: 9,
            ..Config::default()
        };
        assert_eq!(required_terms(&c, 3), 3);
        assert_eq!(required_terms(&c, 20), 9);
    }
}
