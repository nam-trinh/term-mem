//! The ruleset: what shipped, and what the user added.
//!
//! docs/plan.md requires "a user rule file, because internal hostname and
//! ticket-ID shapes are site-specific and no shipped ruleset will guess them".
//! That file is `~/.config/term-mem/redact.toml`, and `$TMEM_CONFIG_DIR`
//! redirects it.

use anyhow::{Context, Result};
use regex::Regex;
use std::ops::Range;
use std::path::PathBuf;

pub struct Rule {
    pub name: String,
    kind: Kind,
}

enum Kind {
    /// A regex. If it has a capture group, group 1 is what gets replaced and
    /// the rest is context worth keeping — `Authorization: Bearer` stays so the
    /// row still says what kind of thing was there.
    Pattern(Regex),
    /// The fallback from docs/tech-stack.md: assignment-shaped, high-entropy,
    /// and mixed enough in character classes to look generated rather than
    /// typed.
    Entropy {
        shape: Regex,
        min_bits: f64,
        min_len: usize,
    },
}

impl Rule {
    pub fn pattern(name: &str, re: &str) -> Result<Rule> {
        Ok(Rule {
            name: name.to_string(),
            kind: Kind::Pattern(
                Regex::new(re).with_context(|| format!("compiling redaction rule '{name}'"))?,
            ),
        })
    }

    /// The range to replace, or `None`.
    pub fn find(&self, text: &str) -> Option<Range<usize>> {
        match &self.kind {
            Kind::Pattern(re) => {
                let caps = re.captures(text)?;
                let m = caps.get(1).or_else(|| caps.get(0))?;
                Some(m.range())
            }
            Kind::Entropy {
                shape,
                min_bits,
                min_len,
            } => {
                for caps in shape.captures_iter(text) {
                    let Some(value) = caps.get(1) else { continue };
                    let v = value.as_str();
                    if v.len() >= *min_len
                        && !looks_like_a_path(v)
                        && looks_generated(v)
                        && shannon_bits(v) >= *min_bits
                    {
                        return Some(value.range());
                    }
                }
                None
            }
        }
    }
}

/// Shannon entropy in bits per character.
fn shannon_bits(s: &str) -> f64 {
    let mut counts = [0usize; 256];
    let bytes = s.as_bytes();
    for b in bytes {
        counts[*b as usize] += 1;
    }
    let len = bytes.len() as f64;
    -counts
        .iter()
        .filter(|c| **c > 0)
        .map(|c| {
            let p = *c as f64 / len;
            p * p.log2()
        })
        .sum::<f64>()
}

/// Is this a filesystem path or a URL rather than a credential?
///
/// The single most important line in this file, and it was written after the
/// fact. On its first contact with a real archive the entropy rule fired 38
/// times and was wrong 38 times: every match was a path assigned to a shell
/// variable, of the shape `S=/private/tmp/…/4d827016-5b15-…/scratchpad`. Long,
/// mixed-case, digit-bearing, genuinely high-entropy — and a path. The rule
/// replaced real command lines with `[redacted:entropy]`, which is precisely
/// the mangling docs/plan.md warns about. See docs/phases/phase-3.md finding 2.
fn looks_like_a_path(s: &str) -> bool {
    // Any `/` at all. The first tightening only rejected values that *began*
    // like a path, and the next run against the same archive was still wrong
    // eight times out of eight — this time on relative paths
    // (`f="tests/fixtures/claude_code/finding-09-many-to-one.jsonl"`).
    //
    // The cost is stated rather than hidden: a base64 credential containing `/`
    // is now invisible to the entropy fallback. That is a deliberate trade. In
    // a developer's archive `/` means path far more often than it means
    // credential, and the pattern rules already cover the slash-bearing secrets
    // that have a known shape. Partially redacting a secret — replacing the
    // longest slash-free run and leaving the rest — was the alternative, and it
    // is worse: it looks redacted and is not.
    s.contains('/')
}

/// Does this look like something a machine produced rather than something a
/// person typed?
///
/// Entropy alone is not enough: `correcthorsebatterystaple` scores respectably
/// and is a passphrase in a doc, not a credential in a log. Requiring more than
/// one character class is what keeps ordinary configuration out — and being
/// wrong here is expensive in both directions, since a false positive silently
/// mangles a real answer.
fn looks_generated(s: &str) -> bool {
    let has_lower = s.bytes().any(|b| b.is_ascii_lowercase());
    let has_upper = s.bytes().any(|b| b.is_ascii_uppercase());
    let has_digit = s.bytes().any(|b| b.is_ascii_digit());
    let classes = [has_lower, has_upper, has_digit]
        .iter()
        .filter(|b| **b)
        .count();
    // Either mixed classes, or long enough to be an unmistakable hex/base64 blob.
    classes >= 3 || (classes >= 2 && s.len() >= 32)
}

pub struct Ruleset(Vec<Rule>);

impl Ruleset {
    pub fn iter(&self) -> impl Iterator<Item = &Rule> {
        self.0.iter()
    }

    /// The rules that run by default: pattern rules only.
    ///
    /// **The entropy fallback is not among them, and that is a departure from
    /// docs/plan.md's Scope.** It is implemented, tested, and one line in the
    /// rule file away — `[entropy] enabled = true` — but it is off, because a
    /// false positive here is not a bad search result. The raw `tool_use` block
    /// is never stored, so replacing a real command line with
    /// `[redacted:entropy]` destroys it permanently, and "capture is
    /// irreversible, retrieval is not" is the roadmap's first principle.
    ///
    /// Measured on a real archive across three successive tightenings: 38 hits,
    /// then 8, then 3 — every one of them a path or a filename, and none of
    /// them a credential. See docs/phases/phase-3.md finding 2.
    pub fn builtin() -> Ruleset {
        let rules = vec![
            // Whole blocks first: the body of a private key is not something to
            // leave behind after replacing its header.
            (
                "pem-private-key",
                r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            ),
            ("anthropic-key", r"sk-ant-[A-Za-z0-9_\-]{16,}"),
            ("openai-key", r"sk-[A-Za-z0-9]{20,}"),
            ("github-token", r"gh[pousr]_[A-Za-z0-9]{20,}"),
            ("gitlab-token", r"glpat-[A-Za-z0-9_\-]{16,}"),
            ("aws-access-key", r"(?:AKIA|ASIA)[0-9A-Z]{16}"),
            ("gcp-api-key", r"AIza[0-9A-Za-z_\-]{35}"),
            ("slack-token", r"xox[abprs]-[A-Za-z0-9\-]{10,}"),
            ("stripe-key", r"[sr]k_(?:live|test)_[A-Za-z0-9]{16,}"),
            ("npm-token", r"npm_[A-Za-z0-9]{32,}"),
            (
                "jwt",
                r"eyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
            ),
            // Keeps the header name, replaces the credential: the row should
            // still record that an Authorization header was involved.
            (
                "auth-header",
                r"(?i)authorization\s*:\s*(?:bearer|basic|token)\s+([A-Za-z0-9._\-+/=]{8,})",
            ),
            (
                "url-credentials",
                r"[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s/@:]+:([^\s/@]{3,})@",
            ),
        ];
        Ruleset(
            rules
                .into_iter()
                .map(|(n, r)| Rule::pattern(n, r).expect("a shipped rule must compile"))
                .collect(),
        )
    }

    /// The pattern rules plus the entropy fallback, as the rule file turns it
    /// on and as the tests exercise it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_entropy(min_bits: f64, min_len: usize) -> Ruleset {
        let mut set = Ruleset::builtin();
        set.0.push(entropy_rule(min_bits, min_len));
        set
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn has_entropy(&self) -> bool {
        self.0.iter().any(|r| r.name == "entropy")
    }

    /// Built-ins, then the user's own. User rules run last so a site rule can
    /// catch what the shipped ones left.
    pub fn load() -> Result<Ruleset> {
        let mut set = Ruleset::builtin();
        let path = user_rules_path()?;
        if !path.exists() {
            return Ok(set);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let file: UserRules =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

        // Opt-in, for the reason on `builtin` above.
        if file.entropy.enabled == Some(true) {
            set.0.push(entropy_rule(
                file.entropy.min_bits.unwrap_or(4.0),
                file.entropy.min_len.unwrap_or(20),
            ));
        }
        // docs/phases/phase-0.md finding 11: the one thing that actually turns
        // up in a real archive is an email address, arriving from ambient
        // context rather than from anything the user pasted. It is off by
        // default because a user's own address is not a secret from them, and
        // redacting every address mangles ordinary discussion.
        if file.builtin.email == Some(true) {
            set.0.push(Rule::pattern(
                "email",
                r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b",
            )?);
        }
        for r in file.rule {
            // A bad user rule is fatal. A redactor the user believes is running
            // and which silently is not is the worst outcome available here.
            set.0.push(
                Rule::pattern(&r.name, &r.pattern).with_context(|| {
                    format!("in {} — fix the rule or remove it", path.display())
                })?,
            );
        }
        Ok(set)
    }
}

fn entropy_rule(min_bits: f64, min_len: usize) -> Rule {
    Rule {
        name: "entropy".to_string(),
        kind: Kind::Entropy {
            // Assignment-shaped, per docs/tech-stack.md. The value charset
            // deliberately excludes `[`, so a replacement cannot re-match.
            shape: Regex::new(
                r#"(?i)[A-Za-z_][A-Za-z0-9_\-]*\s*[:=]\s*["']?([A-Za-z0-9+/_\-.]{16,})["']?"#,
            )
            .expect("the entropy shape must compile"),
            min_bits,
            min_len,
        },
    }
}

pub fn user_rules_path() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("TMEM_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("redact.toml"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set; cannot locate the term-mem config directory")?;
    Ok(home.join(".config/term-mem/redact.toml"))
}

#[derive(serde::Deserialize, Default)]
struct UserRules {
    #[serde(default)]
    rule: Vec<UserRule>,
    #[serde(default)]
    entropy: EntropyConfig,
    #[serde(default)]
    builtin: BuiltinConfig,
}

#[derive(serde::Deserialize)]
struct UserRule {
    name: String,
    pattern: String,
}

#[derive(serde::Deserialize, Default)]
struct EntropyConfig {
    enabled: Option<bool>,
    min_bits: Option<f64>,
    min_len: Option<usize>,
}

#[derive(serde::Deserialize, Default)]
struct BuiltinConfig {
    email: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of these is a real false positive from the first run of this
    /// rule against a real archive, or a near neighbour of one.
    #[test]
    fn paths_are_not_credentials() {
        assert!(looks_like_a_path(
            "/private/tmp/claude-501/-Users-dev-codes-term-mem/4d827016-5b15-4de5-87ed/scratchpad"
        ));
        assert!(looks_like_a_path("~/src/term-mem/target/debug"));
        assert!(looks_like_a_path("./target/release/tmem"));
        assert!(looks_like_a_path("https://example.com/a/b"));
        // The second round of false positives: relative, no leading marker.
        assert!(looks_like_a_path(
            "tests/fixtures/claude_code/finding-09-many-to-one.jsonl"
        ));
        assert!(looks_like_a_path(
            "target/release/deps/budget-f508d50c9affe842"
        ));
        // The stated cost: a slash-bearing base64 credential is out of reach of
        // the entropy rule, and belongs to a pattern rule or to `forget`.
        assert!(looks_like_a_path("Xq7v/Np2LmR9tYbW4zFgH6jKd3sAe8cUn"));
        // Slash-free credentials are what this rule is for.
        assert!(!looks_like_a_path("Xq7vNp2LmR9tYbW4zFgH6jKd3sAe8cUn"));
    }

    #[test]
    fn entropy_separates_generated_from_typed() {
        assert!(shannon_bits("Xq7vNp2LmR9tYbW4zFgH6jKd3sAe8cUn") > 4.0);
        // A passphrase in a document scores well and is not a credential; the
        // character-class test is what keeps it out.
        assert!(!looks_generated("correcthorsebatterystaple"));
        assert!(!looks_generated("changeme"));
        assert!(looks_generated("Xq7vNp2LmR9tYbW4zFgH6jKd3sAe8cUn"));
        // Two classes is enough once it is unmistakably a blob.
        assert!(looks_generated(&"a1".repeat(16)));
        assert!(!looks_generated(&"a1".repeat(4)));
    }

    #[test]
    fn a_rule_with_a_group_replaces_only_the_group() {
        let r = Rule::pattern("auth", r"(?i)authorization:\s*bearer\s+(\S+)").unwrap();
        let text = "Authorization: Bearer abc123xyz";
        let range = r.find(text).unwrap();
        assert_eq!(&text[range], "abc123xyz");
    }

    #[test]
    fn every_shipped_rule_compiles() {
        assert!(Ruleset::builtin().iter().count() > 10);
    }

    /// The default must not include the rule that destroys real content when it
    /// is wrong, and it must be reachable when asked for.
    #[test]
    fn entropy_is_off_by_default_and_available_on_request() {
        assert!(!Ruleset::builtin().has_entropy());
        assert!(Ruleset::with_entropy(4.0, 20).has_entropy());
    }
}
