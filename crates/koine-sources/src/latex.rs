//! LaTeX-to-Unicode decoding for the *query* side of the scorer.
//!
//! A port of `im_bibtex::decode_latex`, the decoder imbib's
//! `resolveStructuredCitation` runs over a citation's authors, title and
//! journal before scoring (never over a candidate). The tables and the pass
//! order are the original's, so the goldens recorded through it reproduce:
//! replacement table longest-pattern-first, then `~` outside URLs, then
//! formatting commands, then brace cleaning.
//!
//! One recorded defect lives here (`citation-scorer-NOTES.md` item 7): the
//! table has `\'{\i}` but no other accent over a dotless i, so `\^{\i}`
//! decodes to `\^ı` with the backslash kept. [`decode_latex_with`] takes the
//! quirk as a flag; [`Quirks::backslash_dotless_i_decode`] is where the
//! scorer sets it.
//!
//! [`Quirks::backslash_dotless_i_decode`]: crate::score::Quirks::backslash_dotless_i_decode

use std::sync::LazyLock;

use regex::Regex;

/// Decode exactly as imbib does today (the legacy table).
pub fn decode_latex(input: &str) -> String {
    decode_latex_with(input, true)
}

/// Decode; with `backslash_dotless_i` false, the accent-over-dotless-i forms
/// the legacy table lacks (`\^{\i}`, `` \`{\i} ``, `\"{\i}` and their
/// unbraced spellings) decode to the composed letter.
pub fn decode_latex_with(input: &str, backslash_dotless_i: bool) -> String {
    let mut result = input.to_string();
    let table: &[(&str, &str)] = if backslash_dotless_i {
        &LEGACY_PATTERNS
    } else {
        &CORRECTED_PATTERNS
    };
    for (pattern, replacement) in table {
        result = result.replace(pattern, replacement);
    }
    result = replace_tilde_outside_urls(&result);
    result = remove_tex_commands(&result);
    clean_braces(&result)
}

const ACCENT_PATTERNS: &[(&str, &str)] = &[
    // Umlaut (diaeresis)
    ("\\\"a", "ä"),
    ("\\\"A", "Ä"),
    ("\\\"e", "ë"),
    ("\\\"E", "Ë"),
    ("\\\"i", "ï"),
    ("\\\"I", "Ï"),
    ("\\\"o", "ö"),
    ("\\\"O", "Ö"),
    ("\\\"u", "ü"),
    ("\\\"U", "Ü"),
    ("\\\"y", "ÿ"),
    ("\\\"Y", "Ÿ"),
    ("\\\"{a}", "ä"),
    ("\\\"{A}", "Ä"),
    ("\\\"{e}", "ë"),
    ("\\\"{E}", "Ë"),
    ("\\\"{i}", "ï"),
    ("\\\"{I}", "Ï"),
    ("\\\"{o}", "ö"),
    ("\\\"{O}", "Ö"),
    ("\\\"{u}", "ü"),
    ("\\\"{U}", "Ü"),
    // Acute accent
    ("\\'a", "á"),
    ("\\'A", "Á"),
    ("\\'e", "é"),
    ("\\'E", "É"),
    ("\\'i", "í"),
    ("\\'I", "Í"),
    ("\\'o", "ó"),
    ("\\'O", "Ó"),
    ("\\'u", "ú"),
    ("\\'U", "Ú"),
    ("\\'y", "ý"),
    ("\\'Y", "Ý"),
    ("\\'{a}", "á"),
    ("\\'{A}", "Á"),
    ("\\'{e}", "é"),
    ("\\'{E}", "É"),
    ("\\'{i}", "í"),
    ("\\'{I}", "Í"),
    ("\\'{\\i}", "í"),
    ("\\'{o}", "ó"),
    ("\\'{O}", "Ó"),
    ("\\'{u}", "ú"),
    ("\\'{U}", "Ú"),
    // Grave accent
    ("\\`a", "à"),
    ("\\`A", "À"),
    ("\\`e", "è"),
    ("\\`E", "È"),
    ("\\`i", "ì"),
    ("\\`I", "Ì"),
    ("\\`o", "ò"),
    ("\\`O", "Ò"),
    ("\\`u", "ù"),
    ("\\`U", "Ù"),
    ("\\`{a}", "à"),
    ("\\`{A}", "À"),
    ("\\`{e}", "è"),
    ("\\`{E}", "È"),
    ("\\`{o}", "ò"),
    ("\\`{O}", "Ò"),
    ("\\`{u}", "ù"),
    ("\\`{U}", "Ù"),
    // Circumflex
    ("\\^a", "â"),
    ("\\^A", "Â"),
    ("\\^e", "ê"),
    ("\\^E", "Ê"),
    ("\\^i", "î"),
    ("\\^I", "Î"),
    ("\\^o", "ô"),
    ("\\^O", "Ô"),
    ("\\^u", "û"),
    ("\\^U", "Û"),
    ("\\^{a}", "â"),
    ("\\^{A}", "Â"),
    ("\\^{e}", "ê"),
    ("\\^{E}", "Ê"),
    ("\\^{o}", "ô"),
    ("\\^{O}", "Ô"),
    ("\\^{u}", "û"),
    ("\\^{U}", "Û"),
    // Tilde
    ("\\~a", "ã"),
    ("\\~A", "Ã"),
    ("\\~n", "ñ"),
    ("\\~N", "Ñ"),
    ("\\~o", "õ"),
    ("\\~O", "Õ"),
    ("\\~{a}", "ã"),
    ("\\~{A}", "Ã"),
    ("\\~{n}", "ñ"),
    ("\\~{N}", "Ñ"),
    ("\\~{o}", "õ"),
    ("\\~{O}", "Õ"),
    // Cedilla
    ("\\c c", "ç"),
    ("\\c C", "Ç"),
    ("\\c{c}", "ç"),
    ("\\c{C}", "Ç"),
    // Ring
    ("\\r a", "å"),
    ("\\r A", "Å"),
    ("\\r{a}", "å"),
    ("\\r{A}", "Å"),
    ("\\aa", "å"),
    ("\\AA", "Å"),
    // Caron
    ("\\v c", "č"),
    ("\\v C", "Č"),
    ("\\v s", "š"),
    ("\\v S", "Š"),
    ("\\v z", "ž"),
    ("\\v Z", "Ž"),
    ("\\v{c}", "č"),
    ("\\v{C}", "Č"),
    ("\\v{s}", "š"),
    ("\\v{S}", "Š"),
    ("\\v{z}", "ž"),
    ("\\v{Z}", "Ž"),
    // Breve
    ("\\u a", "ă"),
    ("\\u A", "Ă"),
    ("\\u{a}", "ă"),
    ("\\u{A}", "Ă"),
    // Macron
    ("\\=a", "ā"),
    ("\\=A", "Ā"),
    ("\\=e", "ē"),
    ("\\=E", "Ē"),
    ("\\=i", "ī"),
    ("\\=I", "Ī"),
    ("\\=o", "ō"),
    ("\\=O", "Ō"),
    ("\\=u", "ū"),
    ("\\=U", "Ū"),
    // Dot above
    ("\\.z", "ż"),
    ("\\.Z", "Ż"),
    ("\\.{z}", "ż"),
    ("\\.{Z}", "Ż"),
    // Ogonek
    ("\\k a", "ą"),
    ("\\k A", "Ą"),
    ("\\k e", "ę"),
    ("\\k E", "Ę"),
    ("\\k{a}", "ą"),
    ("\\k{A}", "Ą"),
    ("\\k{e}", "ę"),
    ("\\k{E}", "Ę"),
    // Stroke
    ("\\l", "ł"),
    ("\\L", "Ł"),
    ("\\o", "ø"),
    ("\\O", "Ø"),
    // Dotless i
    ("\\i", "ı"),
    ("{\\i}", "ı"),
];

/// What the legacy table lacks: every other accent over a dotless i.
const DOTLESS_I_ACCENTS: &[(&str, &str)] = &[
    ("\\^{\\i}", "î"),
    ("\\^\\i", "î"),
    ("\\`{\\i}", "ì"),
    ("\\`\\i", "ì"),
    ("\\\"{\\i}", "ï"),
    ("\\\"\\i", "ï"),
    ("\\'\\i", "í"),
];

const SPECIAL_CHARACTERS: &[(&str, &str)] = &[
    // Ligatures
    ("\\ae", "æ"),
    ("\\AE", "Æ"),
    ("\\oe", "œ"),
    ("\\OE", "Œ"),
    ("\\ss", "ß"),
    ("{\\ae}", "æ"),
    ("{\\AE}", "Æ"),
    ("{\\oe}", "œ"),
    ("{\\OE}", "Œ"),
    ("{\\ss}", "ß"),
    // Punctuation
    ("---", "—"),
    ("--", "–"),
    ("``", "\u{201C}"),
    ("''", "\u{201D}"),
    ("`", "\u{2018}"),
    // Common symbols
    ("\\&", "&"),
    ("\\%", "%"),
    ("\\$", "$"),
    ("\\#", "#"),
    ("\\_", "_"),
    ("\\{", "{"),
    ("\\}", "}"),
    ("\\textasciitilde", "~"),
    ("\\textbackslash", "\\"),
    ("\\copyright", "©"),
    ("\\texttrademark", "™"),
    ("\\textregistered", "®"),
    ("\\pounds", "£"),
    ("\\euro", "€"),
    ("\\yen", "¥"),
    ("\\S", "§"),
    ("\\P", "¶"),
    ("\\dag", "†"),
    ("\\ddag", "‡"),
    ("\\textdagger", "†"),
    ("\\textdaggerdbl", "‡"),
    ("\\textbullet", "•"),
    ("\\ldots", "…"),
    ("\\dots", "…"),
    ("\\textellipsis", "…"),
];

const MATH_SYMBOLS: &[(&str, &str)] = &[
    ("\\alpha", "α"),
    ("\\beta", "β"),
    ("\\gamma", "γ"),
    ("\\delta", "δ"),
    ("\\epsilon", "ε"),
    ("\\zeta", "ζ"),
    ("\\eta", "η"),
    ("\\theta", "θ"),
    ("\\iota", "ι"),
    ("\\kappa", "κ"),
    ("\\lambda", "λ"),
    ("\\mu", "μ"),
    ("\\nu", "ν"),
    ("\\xi", "ξ"),
    ("\\pi", "π"),
    ("\\rho", "ρ"),
    ("\\sigma", "σ"),
    ("\\tau", "τ"),
    ("\\upsilon", "υ"),
    ("\\phi", "φ"),
    ("\\chi", "χ"),
    ("\\psi", "ψ"),
    ("\\omega", "ω"),
    ("\\Gamma", "Γ"),
    ("\\Delta", "Δ"),
    ("\\Theta", "Θ"),
    ("\\Lambda", "Λ"),
    ("\\Xi", "Ξ"),
    ("\\Pi", "Π"),
    ("\\Sigma", "Σ"),
    ("\\Upsilon", "Υ"),
    ("\\Phi", "Φ"),
    ("\\Psi", "Ψ"),
    ("\\Omega", "Ω"),
    ("\\times", "×"),
    ("\\div", "÷"),
    ("\\pm", "±"),
    ("\\mp", "∓"),
    ("\\cdot", "·"),
    ("\\ast", "∗"),
    ("\\star", "⋆"),
    ("\\circ", "∘"),
    ("\\leq", "≤"),
    ("\\geq", "≥"),
    ("\\neq", "≠"),
    ("\\approx", "≈"),
    ("\\equiv", "≡"),
    ("\\sim", "∼"),
    ("\\propto", "∝"),
    ("\\ll", "≪"),
    ("\\gg", "≫"),
    ("\\in", "∈"),
    ("\\notin", "∉"),
    ("\\subset", "⊂"),
    ("\\supset", "⊃"),
    ("\\subseteq", "⊆"),
    ("\\supseteq", "⊇"),
    ("\\cup", "∪"),
    ("\\cap", "∩"),
    ("\\emptyset", "∅"),
    ("\\to", "→"),
    ("\\rightarrow", "→"),
    ("\\leftarrow", "←"),
    ("\\leftrightarrow", "↔"),
    ("\\Rightarrow", "⇒"),
    ("\\Leftarrow", "⇐"),
    ("\\Leftrightarrow", "⇔"),
    ("\\infty", "∞"),
    ("\\partial", "∂"),
    ("\\nabla", "∇"),
    ("\\forall", "∀"),
    ("\\exists", "∃"),
    ("\\neg", "¬"),
    ("\\wedge", "∧"),
    ("\\vee", "∨"),
    ("\\sum", "∑"),
    ("\\prod", "∏"),
    ("\\int", "∫"),
    ("\\sqrt", "√"),
];

/// Accents, specials and math, longest pattern first (a stable sort, as the
/// original: equal-length patterns keep table order).
fn combined(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut patterns: Vec<(&str, &str)> = Vec::new();
    patterns.extend(extra.iter().copied());
    patterns.extend(ACCENT_PATTERNS.iter().copied());
    patterns.extend(SPECIAL_CHARACTERS.iter().copied());
    patterns.extend(MATH_SYMBOLS.iter().copied());
    patterns.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
    patterns
}

static LEGACY_PATTERNS: LazyLock<Vec<(&str, &str)>> = LazyLock::new(|| combined(&[]));
static CORRECTED_PATTERNS: LazyLock<Vec<(&str, &str)>> =
    LazyLock::new(|| combined(DOTLESS_I_ACCENTS));

static FORMATTING_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\\textbf\{([^}]*)\}",
        r"\\textit\{([^}]*)\}",
        r"\\textrm\{([^}]*)\}",
        r"\\texttt\{([^}]*)\}",
        r"\\textsf\{([^}]*)\}",
        r"\\textsc\{([^}]*)\}",
        r"\\emph\{([^}]*)\}",
        r"\\underline\{([^}]*)\}",
        r"\\mbox\{([^}]*)\}",
        r"\\text\{([^}]*)\}",
        r"\\mathrm\{([^}]*)\}",
        r"\\mathit\{([^}]*)\}",
        r"\\mathbf\{([^}]*)\}",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static regex"))
    .collect()
});
static GENERIC_COMMAND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\[a-zA-Z]+\{([^}]*)\}").expect("static regex"));
static EMPTY_BRACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{\}").expect("static regex"));
static SINGLE_CHAR_BRACES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{([^{}])\}").expect("static regex"));
static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)https?://\S+|ftp://\S+").expect("static regex"));

fn replace_tilde_outside_urls(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last = 0;
    for m in URL_RE.find_iter(input) {
        result.push_str(&input[last..m.start()].replace('~', " "));
        result.push_str(m.as_str());
        last = m.end();
    }
    result.push_str(&input[last..].replace('~', " "));
    result
}

fn remove_tex_commands(input: &str) -> String {
    let mut result = input.to_string();
    for pattern in FORMATTING_PATTERNS.iter() {
        result = pattern.replace_all(&result, "$1").to_string();
    }
    GENERIC_COMMAND.replace_all(&result, "$1").to_string()
}

fn clean_braces(input: &str) -> String {
    let result = EMPTY_BRACES.replace_all(input, "").to_string();
    SINGLE_CHAR_BRACES.replace_all(&result, "$1").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_original_decoder() {
        assert_eq!(decode_latex(r#"M\"uller"#), "Müller");
        assert_eq!(decode_latex(r#"M\"{u}ller"#), "Müller");
        assert_eq!(decode_latex(r#"caf\'{e}"#), "café");
        assert_eq!(decode_latex(r#"Smith \& Jones"#), "Smith & Jones");
        assert_eq!(decode_latex("pages 1--10"), "pages 1–10");
        assert_eq!(decode_latex(r#"\textbf{bold}"#), "bold");
        assert_eq!(decode_latex("{DNA}"), "{DNA}");
        assert_eq!(decode_latex("{a}"), "a");
        assert_eq!(decode_latex("test{}"), "test");
        assert_eq!(
            decode_latex(r#"M\"uller, J. and Garc\'{\i}a, M."#),
            "Müller, J. and García, M."
        );
    }

    /// NOTES item 7, pinned: the legacy table leaves the backslash.
    #[test]
    fn dotless_i_under_circumflex_is_the_recorded_defect() {
        assert_eq!(decode_latex(r#"Lema\^{\i}tre"#), "Lema\\^ıtre");
        assert_eq!(decode_latex_with(r#"Lema\^{\i}tre"#, false), "Lemaître");
        assert_eq!(decode_latex_with(r#"na\"{\i}ve"#, false), "naïve");
    }
}
