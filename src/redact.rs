//! The one credential masker every persisted or printed view goes through: the
//! hook log, the failure tee, recordings and the transcript audits. Add a masking
//! rule here and every writer inherits it.

/// Mask credential shapes before anything from a transcript is printed or
/// serialized. This report is routinely piped into logs and pastes, and
/// transcripts carry `https://<token>@host`, `Authorization: Bearer …` and
/// `token=…` verbatim — `scan-secrets` redacts by default, so must this.
pub fn redact_credentials(s: &str) -> String {
    use std::sync::OnceLock;
    static RULES: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        [
            // userinfo in a URL: https://user:token@host
            r"(?i)://[^/\s@]+@",
            // Authorization / api-key headers. The value runs to the end of the
            // header, not to the end of the first word: `\S+` stopped at
            // `Bearer` and left the token itself in the clear
            // (`Authorization: [REDACTED] sk-live-…`). Bounded by quote/newline
            // so `-H "Authorization: Bearer x"` does not swallow the rest of the
            // command line.
            r#"(?i)(authorization|x-api-key|api-key)\s*:\s*[^\r\n"']+"#,
            r"(?i)\bbearer\s+[A-Za-z0-9._\-]+",
            // token=… / key=… / password=… in query strings or env assignments
            r#"(?i)\b[A-Za-z0-9_]*(token|api[_-]?key|secret|password|passwd|pwd)(?:_[A-Za-z0-9_]*)?\s*[=:]\s*[^\s&"']+"#,
            // PEM blocks: header, base64 body and footer (or end of input if cut)
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
        ]
        .iter()
        .filter_map(|p| regex::Regex::new(p).ok())
        .collect()
    });
    let mut out = s.to_string();
    for (i, re) in rules.iter().enumerate() {
        out = match i {
            0 => re.replace_all(&out, "://[REDACTED]@").into_owned(),
            _ => re.replace_all(&out, "[REDACTED]").into_owned(),
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_removes_the_whole_authorization_value() {
        // Regression: `\S+` ended the match at `Bearer`, so the token survived as
        // `Authorization: [REDACTED] sk-live-…` in every redacted report.
        let out = redact_credentials(
            "curl -H \"Authorization: Bearer sk-live-abc123\" https://api.example.com",
        );
        assert!(!out.contains("sk-live-abc123"), "{out}");
        // The rest of the command line must stay readable.
        assert!(out.contains("curl -H"), "{out}");
        assert!(out.contains("https://api.example.com"), "{out}");
    }

    #[test]
    fn redaction_covers_header_variants_and_url_userinfo() {
        // Assembled at runtime rather than spelled out: a literal api-key-shaped
        // string in the source trips this repo's own gitleaks gate, and a
        // fingerprint exemption would pin the fixture to one commit hash.
        let value = format!("abcdef{}", 123_456);
        for header in ["x-api-key: ", "API-Key:", "Authorization: Basic "] {
            let input = format!("{header}{value}");
            let out = redact_credentials(&input);
            assert!(
                out.contains("[REDACTED]") && !out.contains(&value),
                "{input} -> {out}"
            );
        }
        let url = redact_credentials("git push https://user:ghp_tok@github.com/o/r.git");
        assert!(!url.contains("ghp_tok"), "{url}");
        assert!(url.contains("://[REDACTED]@github.com"), "{url}");
    }

    #[test]
    fn redaction_leaves_ordinary_text_alone() {
        let plain = "cargo test --locked && git status --short";
        assert_eq!(redact_credentials(plain), plain);
    }

    #[test]
    fn redacts_prefixed_env_style_credential_names() {
        for (input, value) in [
            ("GITHUB_TOKEN=ghp_FAKEFAKEFAKE", "ghp_FAKEFAKEFAKE"),
            ("export OPENAI_API_KEY=sk-FAKE1234", "sk-FAKE1234"),
            ("AWS_SECRET_ACCESS_KEY=FAKEFAKE/abc", "FAKEFAKE/abc"),
            ("DB_PASSWORD=hunter2fake", "hunter2fake"),
        ] {
            let out = redact_credentials(input);
            assert!(
                out.contains("[REDACTED]") && !out.contains(value),
                "{input} -> {out}"
            );
        }
    }

    #[test]
    fn redacts_whole_pem_private_key_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nAAAAAAAAAAAAAAAA\nAAAAAAAAAAAAAAAA\n-----END RSA PRIVATE KEY-----"; // gitleaks:allow synthetic test fixture
        let out = redact_credentials(&format!("key:\n{pem}\nafter"));
        assert!(!out.contains("AAAA") && !out.contains("END RSA"), "{out}");
        assert!(out.ends_with("\nafter"), "{out}");
        // Truncated block (no END marker): body still goes.
        let cut = redact_credentials("-----BEGIN PRIVATE KEY-----\nAAAAAAAA\nAAAAAAAA");
        assert!(!cut.contains("AAAA"), "{cut}");
    }

    #[test]
    fn redaction_near_misses_stay_untouched() {
        for plain in [
            "the tokenizer splits words",
            "TOKEN_LIMIT reached",
            "secretary: on leave",
        ] {
            assert_eq!(redact_credentials(plain), plain);
        }
    }
}
