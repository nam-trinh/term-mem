//! Redaction on the way in.
//!
//! docs/plan.md: "applied **pre-write**. A redactor that runs after the insert
//! has already lost." So this runs against a `ParsedExchange` before anything
//! reaches the database — not against a row, and not in a later pass.
//!
//! Two layers, in the order docs/tech-stack.md gives them:
//!
//! 1. **Pattern rules** for credentials with a known shape.
//! 2. **An entropy fallback** over assignment-shaped tokens, for the shapes no
//!    rule knows.
//!
//! And one thing neither layer is: a guarantee. `tmem forget` is the valve for
//! what this misses, and the scope note in plan.md is explicit that prevention
//! does not replace it. The honest claim is "this catches credentials that look
//! like credentials".

mod rules;

#[cfg(test)]
pub use rules::Rule;
pub use rules::{user_rules_path, Ruleset};

/// What one pass over an exchange changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Rule name to number of replacements, for `status` and for the capture
    /// report. Silent redaction leaves the user unable to tell a mangled
    /// response from a bad one.
    pub hits: Vec<(String, usize)>,
}

impl Report {
    /// Total replacements. Used by tests and by the capture report's detail
    /// line; kept even where a build does not reach it, because a count nobody
    /// can see is how silent redaction starts.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn total(&self) -> usize {
        self.hits.iter().map(|(_, n)| n).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }
    fn add(&mut self, name: &str, n: usize) {
        if n == 0 {
            return;
        }
        match self.hits.iter_mut().find(|(r, _)| r == name) {
            Some((_, c)) => *c += n,
            None => self.hits.push((name.to_string(), n)),
        }
    }
    pub fn merge(&mut self, other: &Report) {
        for (name, n) in &other.hits {
            self.add(name, *n);
        }
    }
    /// `aws-key ×2, entropy ×1`
    pub fn summary(&self) -> String {
        self.hits
            .iter()
            .map(|(r, n)| {
                if *n == 1 {
                    r.clone()
                } else {
                    format!("{r} ×{n}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub struct Redactor {
    rules: Ruleset,
}

impl Redactor {
    pub fn new(rules: Ruleset) -> Self {
        Self { rules }
    }

    /// Load the shipped rules plus whatever the user added. A malformed user
    /// rule file is an error, not a warning that scrolls past: a redactor the
    /// user believes is running and isn't is worse than no redactor.
    pub fn load() -> anyhow::Result<Self> {
        Ok(Self::new(Ruleset::load()?))
    }

    /// Redact in place. Returns what changed.
    pub fn scrub(&self, text: &mut String) -> Report {
        let mut report = Report::default();
        for rule in self.rules.iter() {
            let mut n = 0;
            let replacement = format!("[redacted:{}]", rule.name);
            while let Some(range) = rule.find(text) {
                // A rule that matches its own replacement would otherwise
                // rewrite the same bytes until the guard below trips: correct
                // output, a nonsense count, and ten thousand full-text scans on
                // a budgeted path. User rules can do this too, so the check
                // belongs here rather than in any one pattern.
                if text[range.clone()] == replacement {
                    break;
                }
                text.replace_range(range, &replacement);
                n += 1;
                if n > 10_000 {
                    break; // pathological input; the row is already unusable
                }
            }
            report.add(&rule.name, n);
        }
        report
    }

    /// Non-mutating form, for callers that have a `&str` and for tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn scrub_str(&self, text: &str) -> (String, Report) {
        let mut owned = text.to_string();
        let report = self.scrub(&mut owned);
        (owned, report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_redactor() -> Redactor {
        Redactor::new(Ruleset::builtin())
    }

    /// The opt-in configuration, for the entropy tests.
    fn entropy_redactor() -> Redactor {
        Redactor::new(Ruleset::with_entropy(4.0, 20))
    }

    /// The shapes docs/tech-stack.md names by hand.
    #[test]
    fn known_credential_shapes_are_replaced() {
        let cases = [
            ("sk-abcdefghijklmnopqrstuvwxyz012345", "openai-key"),
            (
                "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "anthropic-key",
            ),
            ("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", "github-token"),
            ("AKIAIOSFODNN7EXAMPLE", "aws-access-key"),
            ("AIzaSyA0123456789012345678901234567890a", "gcp-api-key"),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
                "jwt",
            ),
        ];
        // Assembled rather than written out: a literal of this shape is a
        // Slack token as far as GitHub's push protection is concerned, and a
        // test fixture is not worth an exception on a secret-scanning rule.
        let slack = format!("xoxb-{}-{}", "1".repeat(12), "abcdefghijklmnop");
        let cases: Vec<(&str, &str)> = cases
            .into_iter()
            .chain(std::iter::once((slack.as_str(), "slack-token")))
            .collect();

        let r = default_redactor();
        for (secret, rule) in cases {
            let (out, report) = r.scrub_str(&format!("here it is: {secret} — use it"));
            assert!(
                !out.contains(secret),
                "{rule} left the secret in place: {out}"
            );
            assert!(out.contains(&format!("[redacted:{rule}]")), "{out}");
            assert_eq!(report.total(), 1, "{rule}: {report:?}");
            // The text around it survives; this is not a blanket delete.
            assert!(out.starts_with("here it is: "), "{out}");
            assert!(out.ends_with(" — use it"), "{out}");
        }
    }

    #[test]
    fn authorization_headers_and_pem_blocks_go_whole() {
        let r = default_redactor();
        let (out, _) = r.scrub_str("curl -H 'Authorization: Bearer abcdef0123456789abcdef' api");
        assert!(!out.contains("abcdef0123456789"), "{out}");

        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\nsecretsecret\n-----END RSA PRIVATE KEY-----";
        let (out, _) = r.scrub_str(&format!("my key is\n{pem}\nplease help"));
        assert!(!out.contains("secretsecret"), "the body must go too: {out}");
        assert!(!out.contains("MIIEowIBAAKCAQEA"), "{out}");
        assert!(out.contains("please help"), "{out}");
    }

    /// docs/tech-stack.md layer 2: "over assignment-shaped tokens, to catch the
    /// shapes no rule knows".
    #[test]
    fn the_entropy_fallback_catches_unknown_shapes() {
        let r = entropy_redactor();
        let (out, report) = r.scrub_str("export DEPLOY_TOKEN=Xq7vNp2LmR9tYbW4zFgH6jKd3sAe8cUn");
        assert!(!out.contains("Xq7vNp2LmR9tYbW4zFgH6jKd3sAe8cUn"), "{out}");
        assert!(
            report.hits.iter().any(|(r, _)| r == "entropy"),
            "{report:?}"
        );
        // The variable name is left, because it is what makes the row readable.
        assert!(out.contains("DEPLOY_TOKEN"), "{out}");
    }

    /// The failure mode that makes redaction hated: a low-entropy assignment is
    /// ordinary configuration, and mangling it loses real content for nothing.
    #[test]
    fn ordinary_assignments_are_left_alone() {
        // Tested with entropy *on*, because these are the lines it would eat.
        let r = entropy_redactor();
        for benign in [
            "export EDITOR=nvim",
            "RUST_LOG=debug cargo test",
            "password = correcthorsebatterystaple",
            "api_key = changeme",
            "let timeout = 30",
            "DATABASE_URL=postgres://localhost:5432/dev",
            // The shapes that made the entropy rule wrong 38 times out of 38
            // on its first real archive.
            "S=/private/tmp/claude-501/-Users-dev-codes-term-mem/4d827016-5b15-4de5-87ed/scratch",
            "export TMEM_HOME=~/.local/share/term-mem/testing-4d827016",
            "out=./target/release/deps/budget-f508d50c9affe842",
            "f=tests/fixtures/claude_code/finding-09-many-to-one.jsonl",
            "p=src/capture/adapters/claude_code.rs",
        ] {
            let (out, report) = r.scrub_str(benign);
            assert_eq!(out, benign, "mangled a benign line: {report:?}");
        }
    }

    /// Code and prose must survive intact. A redactor that eats a base64 blob
    /// in a code fence is one people turn off.
    #[test]
    fn prose_and_code_survive() {
        let r = entropy_redactor();
        let text = "The commit is 3f2a1b9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a and the \
                    file is src/capture/adapters/claude_code.rs. Run cargo test --release.";
        let (out, report) = r.scrub_str(text);
        assert_eq!(out, text, "{report:?}");
    }

    /// The guard in `scrub`, exercised by a rule that deliberately matches its
    /// own replacement. The shipped `url-credentials` rule used to do this by
    /// accident and was fixed in the pattern — but a *user* rule can do it too,
    /// and no shipped regex controls those. Without the guard this does not
    /// terminate until the 10,000-iteration cap, and reports ×10001.
    #[test]
    fn a_self_matching_rule_stops_instead_of_spinning() {
        let mut set = Ruleset::builtin();
        set.push(Rule::pattern("greedy", r"\[redacted:greedy\]|secretvalue").unwrap());
        let r = Redactor::new(set);
        let (out, report) = r.scrub_str("the value is secretvalue ok");
        assert_eq!(out, "the value is [redacted:greedy] ok");
        assert_eq!(
            report.hits,
            vec![("greedy".to_string(), 1)],
            "the rule rewrote its own output: {report:?}"
        );
    }

    /// `url-credentials` used to match its own `[redacted:url-credentials]`
    /// output: the text converged, so the result looked right, but `scrub`
    /// rewrote the same bytes until the loop guard tripped — ten thousand
    /// full-text regex scans on the ingest path, and a report saying `×10001`.
    #[test]
    fn a_rule_cannot_loop_on_its_own_replacement() {
        let r = default_redactor();
        let (out, report) = r.scrub_str("psql postgres://admin:hunter2pass@db.example.com/app now");
        assert!(!out.contains("hunter2pass"), "{out}");
        assert_eq!(
            report.hits,
            vec![("url-credentials".to_string(), 1)],
            "one credential, one replacement: {report:?}"
        );
        assert!(out.starts_with("psql postgres://admin:"), "{out}");
        assert!(out.ends_with("@db.example.com/app now"), "{out}");
    }

    #[test]
    fn several_secrets_in_one_exchange_are_all_replaced_and_all_counted() {
        let r = default_redactor();
        let (out, report) = r.scrub_str(
            "first ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 then \
             AKIAIOSFODNN7EXAMPLE then AKIAIOSFODNN7FAKEKEY",
        );
        assert!(!out.contains("ghp_"), "{out}");
        assert!(!out.contains("AKIA"), "{out}");
        assert_eq!(report.total(), 3, "{report:?}");
        assert_eq!(report.summary(), "github-token, aws-access-key ×2");
    }
}
