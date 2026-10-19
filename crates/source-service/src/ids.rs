//! Canonical identifiers (S-D4 `ids`): parsing one, and scanning a reference
//! string for any. The spellings accepted are the ones people paste: a bare
//! DOI, `doi:`, a doi.org URL; an arXiv id old or new, `arXiv:`, an
//! arxiv.org URL; `pmid:`, `pmcid:`, `bibcode:`.

/// One canonical identifier, `kind` lower-case (`doi`, `arxiv`, `pmid`,
/// `pmcid`, `bibcode`, `isbn`, `openalex`, `s2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
    pub kind: String,
    pub value: String,
}

impl Identifier {
    pub fn new(kind: &str, value: &str) -> Self {
        Identifier {
            kind: kind.into(),
            value: value.into(),
        }
    }

    /// `doi:10.1126/science.1063991`.
    pub fn canonical(&self) -> String {
        format!("{}:{}", self.kind, self.value)
    }
}

fn is_doi(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("10.") else {
        return false;
    };
    let Some(slash) = rest.find('/') else {
        return false;
    };
    let prefix = &rest[..slash];
    !prefix.is_empty()
        && prefix.chars().all(|c| c.is_ascii_digit() || c == '.')
        && rest.len() > slash + 1
}

fn is_arxiv_new(s: &str) -> bool {
    // YYMM.NNNNN with an optional vN.
    let core = s.split('v').next().unwrap_or(s);
    let Some((a, b)) = core.split_once('.') else {
        return false;
    };
    a.len() == 4
        && a.chars().all(|c| c.is_ascii_digit())
        && (4..=5).contains(&b.len())
        && b.chars().all(|c| c.is_ascii_digit())
}

fn is_arxiv_old(s: &str) -> bool {
    // archive[.SC]/YYMMNNN
    let Some((archive, num)) = s.split_once('/') else {
        return false;
    };
    let num = num.split('v').next().unwrap_or(num);
    !archive.is_empty()
        && archive
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '-' || c == '.')
        && num.len() == 7
        && num.chars().all(|c| c.is_ascii_digit())
}

fn trim_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(['.', ',', ';', ')', ']', '>', '"', '\''])
}

/// Parse one identifier as pasted. `None` when the text is not one.
pub fn parse(text: &str) -> Option<Identifier> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    for (kind, prefixes) in [
        (
            "doi",
            &[
                "doi:",
                "https://doi.org/",
                "http://doi.org/",
                "https://dx.doi.org/",
                "http://dx.doi.org/",
                "doi.org/",
            ][..],
        ),
        (
            "arxiv",
            &[
                "arxiv:",
                "https://arxiv.org/abs/",
                "http://arxiv.org/abs/",
                "arxiv.org/abs/",
                "https://arxiv.org/pdf/",
            ][..],
        ),
        ("pmid", &["pmid:", "https://pubmed.ncbi.nlm.nih.gov/"][..]),
        ("pmcid", &["pmcid:", "pmc:"][..]),
        (
            "bibcode",
            &["bibcode:", "https://ui.adsabs.harvard.edu/abs/"][..],
        ),
        ("openalex", &["openalex:", "https://openalex.org/"][..]),
        ("s2", &["s2:", "semanticscholar:"][..]),
        ("isbn", &["isbn:"][..]),
    ] {
        for p in prefixes {
            if let Some(rest) = lower.strip_prefix(p) {
                let value =
                    trim_trailing_punct(t[t.len() - rest.len()..].trim().trim_end_matches('/'));
                if value.is_empty() {
                    return None;
                }
                let value = match kind {
                    "arxiv" => value.trim_end_matches(".pdf").to_string(),
                    // `…/abs/<bibcode>/abstract` is what the ADS UI puts in the address bar.
                    "bibcode" => value.trim_end_matches("/abstract").to_string(),
                    "pmcid" => format!("PMC{}", value.trim_start_matches("PMC")),
                    _ => value.to_string(),
                };
                return Some(Identifier::new(kind, &value));
            }
        }
    }
    let bare = trim_trailing_punct(t);
    if is_doi(bare) {
        return Some(Identifier::new("doi", bare));
    }
    if is_arxiv_new(bare) || is_arxiv_old(bare) {
        return Some(Identifier::new("arxiv", bare));
    }
    if bare.len() == 19
        && bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '&')
        && bare[..4].chars().all(|c| c.is_ascii_digit())
    {
        return Some(Identifier::new("bibcode", bare));
    }
    None
}

/// Every identifier found in a reference string, in order of appearance.
pub fn scan(text: &str) -> Vec<Identifier> {
    let mut out: Vec<Identifier> = Vec::new();
    for token in text.split(|c: char| {
        c.is_whitespace() || c == '(' || c == '[' || c == '<' || c == ',' || c == ';'
    }) {
        if let Some(id) = parse(token) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dois_in_every_pasted_spelling() {
        for text in [
            "10.1126/science.1063991",
            "doi:10.1126/science.1063991",
            "DOI:10.1126/science.1063991",
            "https://doi.org/10.1126/science.1063991",
            "http://dx.doi.org/10.1126/science.1063991.",
        ] {
            let id = parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(id.kind, "doi");
            assert_eq!(id.value, "10.1126/science.1063991", "{text}");
        }
        assert_eq!(parse("10.x/y"), None);
        assert_eq!(parse("Abel 2002"), None);
    }

    #[test]
    fn arxiv_ids_old_and_new() {
        assert_eq!(parse("2101.00001").unwrap().value, "2101.00001");
        assert_eq!(parse("arXiv:2101.00001v2").unwrap().value, "2101.00001v2");
        assert_eq!(parse("astro-ph/0112088").unwrap().kind, "arxiv");
        assert_eq!(
            parse("https://arxiv.org/abs/2101.00001").unwrap().kind,
            "arxiv"
        );
        assert_eq!(parse("2002Sci...295...93A").unwrap().kind, "bibcode");
        let ui = parse("https://ui.adsabs.harvard.edu/abs/2002Sci...295...93A/abstract").unwrap();
        assert_eq!(
            (ui.kind.as_str(), ui.value.as_str()),
            ("bibcode", "2002Sci...295...93A")
        );
    }

    #[test]
    fn a_reference_string_is_scanned() {
        let ids = scan("Abel, T. et al. (2002), Science 295, 93, doi:10.1126/science.1063991; see also arXiv:astro-ph/0112088.");
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].canonical(), "doi:10.1126/science.1063991");
        assert_eq!(ids[1].canonical(), "arxiv:astro-ph/0112088");
        assert!(scan("no ids here").is_empty());
    }
}
