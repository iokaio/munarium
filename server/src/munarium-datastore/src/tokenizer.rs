// SPDX-License-Identifier: Apache-2.0
//! The Munarium classifying tokenizer — the analyzer-parity fix.
//!
//! `SimpleTokenizer` splits on every non-alphanumeric character, and the
//! measured divergence from the PostgreSQL oracle was ONE cause: PostgreSQL's
//! parser recognises *structures* where a character-class split sees separate
//! words (see the lexical-compat fixture beside these tests). This tokenizer implements the
//! classes the corpora actually exercise, exactly as the oracle records them:
//!
//! - **Numbers**: unsigned, signed (`-2025`, `+3.5`), decimal (`900000.50` —
//!   and `900000.5` stays a DIFFERENT token, which is the evidence-identity
//!   property the money fixtures exist for), dotted chains (`4.2.1`,
//!   `198.51.100.140`), scientific (`1.2e-9`).
//! - **Sign absorption**: `-`/`+` immediately followed by a digit starts a
//!   signed number wherever it stands — which is precisely how `CVE-2025-31337`
//!   becomes `cve · -2025 · -31337` and `ABC-1234-XY` becomes
//!   `abc · -1234 · xy`, matching the oracle token for token.
//! - **Digit/slash joins** (`file`): `07/638,337` → `07/638` + `337`, the
//!   patent-serial shape.
//! - **Letter-digit dotted chains** (`file`): `v4.2.1` stays whole.
//! - **Alphanumeric words** (`numword`): `US7654321B2`, `rc2` stay whole, as
//!   they always did.
//! - **Hyphenated compounds**: `well-established` emits the compound AND its
//!   parts, each at its own position — the oracle's
//!   `asciihword`/`hword_asciipart` layout. Only all-letter chains compound;
//!   a digit part breaks the chain into word + signed number + word.
//!
//! ## What is deliberately NOT implemented, and where that is recorded
//!
//! URLs, hosts, emails and filesystem paths stay split into words. The plan
//! (lexical-parity.md, "What this means for the plan") says to implement the
//! classes the corpora exercise and accept the rest in writing; the accepted
//! set and the reasoning live in that document's accepted-differences section.
//! The one position constraint that matters — demotion rules read positions —
//! holds internally, because index and query pass through this same tokenizer.
//!
//! ## Identity
//!
//! Changing the analyzer changes what an index contains, so
//! [`ANALYZER_CONTRACT_VERSION`](crate::lexical::ANALYZER_CONTRACT_VERSION)
//! was bumped with it: a build spec carrying the new contract hashes to a new
//! identity, exactly as §5.1 demands of an analyzer change.

use tantivy::tokenizer::{Token, TokenFilter, TokenStream, Tokenizer};

/// Snowball stemming for WORDS only — the pg dictionary split, as a filter.
///
/// PostgreSQL routes `asciiword`/`asciihword` through `english_stem` and every
/// number-bearing class (`uint`, `int`, `float`, `version`, `numword`, `file`)
/// through `simple`, which does not stem. Tantivy's stock `Stemmer` stems
/// everything, and on an opaque token that happens to end in a stemmable
/// suffix that CHANGES it — the md5 fixture `…ecf8427e` lost its final `e`.
/// This filter applies the same Snowball algorithm Tantivy uses, guarded by
/// the same rule the oracle follows: a token carrying an ASCII digit is not a
/// word, and is not stemmed.
#[derive(Clone)]
pub struct WordOnlyStemmer;

impl TokenFilter for WordOnlyStemmer {
    type Tokenizer<T: Tokenizer> = WordOnlyStemmerFilter<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> WordOnlyStemmerFilter<T> {
        WordOnlyStemmerFilter { inner: tokenizer }
    }
}

#[derive(Clone)]
pub struct WordOnlyStemmerFilter<T> {
    inner: T,
}

impl<T: Tokenizer> Tokenizer for WordOnlyStemmerFilter<T> {
    type TokenStream<'a> = WordOnlyStemmerStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        WordOnlyStemmerStream {
            tail: self.inner.token_stream(text),
            stemmer: rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English),
        }
    }
}

pub struct WordOnlyStemmerStream<T> {
    tail: T,
    stemmer: rust_stemmers::Stemmer,
}

impl<T: TokenStream> TokenStream for WordOnlyStemmerStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let token = self.tail.token_mut();
        if token.text.bytes().any(|b| b.is_ascii_digit()) {
            return true;
        }
        if let std::borrow::Cow::Owned(stemmed) = self.stemmer.stem(&token.text) {
            token.text = stemmed;
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// The classifying tokenizer. Scans once per document into a token list; a
/// streaming scanner is an optimization to take after profiling, not before.
#[derive(Clone, Default)]
pub struct MunariumTokenizer;

pub struct MunariumTokenStream {
    tokens: Vec<Token>,
    index: usize,
}

impl Tokenizer for MunariumTokenizer {
    type TokenStream<'a> = MunariumTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> MunariumTokenStream {
        MunariumTokenStream {
            tokens: scan(text),
            index: 0,
        }
    }
}

impl TokenStream for MunariumTokenStream {
    fn advance(&mut self) -> bool {
        if self.index < self.tokens.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.index - 1]
    }
}

/// One pass over the text. Byte offsets are what Tantivy stores; positions
/// count emitted tokens, compounds and their parts each taking one — the same
/// positional layout the oracle's tsvector shows.
fn scan(text: &str) -> Vec<Token> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let byte_end = |i: usize| -> usize { chars.get(i).map(|&(b, _)| b).unwrap_or(text.len()) };

    let mut tokens: Vec<Token> = Vec::new();
    let mut position = 0usize;
    let mut push = |start_byte: usize, end_byte: usize, text: &str, position: &mut usize| {
        tokens.push(Token {
            offset_from: start_byte,
            offset_to: end_byte,
            position: *position,
            text: text.to_string(),
            position_length: 1,
        });
        *position += 1;
    };

    let is_word = |c: char| c.is_alphanumeric();
    let at_digit = |i: usize| i < n && chars[i].1.is_ascii_digit();
    let at_letter =
        |i: usize| i < n && chars[i].1.is_alphanumeric() && !chars[i].1.is_ascii_digit();

    // Consume an alphanumeric run starting at i; returns the exclusive end.
    let run_end = |mut i: usize| -> usize {
        while i < n && is_word(chars[i].1) {
            i += 1;
        }
        i
    };
    let run_has_digit =
        |a: usize, b: usize| -> bool { chars[a..b].iter().any(|&(_, c)| c.is_ascii_digit()) };
    let run_all_letters =
        |a: usize, b: usize| -> bool { chars[a..b].iter().all(|&(_, c)| !c.is_ascii_digit()) };

    let mut i = 0usize;
    while i < n {
        let c = chars[i].1;

        // A sign immediately followed by a digit starts a signed number —
        // wherever it stands. This single rule IS the cve / hyphenated-num
        // behaviour the oracle records.
        if (c == '-' || c == '+') && at_digit(i + 1) {
            let start = i;
            let end = consume_number(&chars, i + 1, n);
            push(
                chars[start].0,
                byte_end(end),
                &text[chars[start].0..byte_end(end)],
                &mut position,
            );
            i = end;
            continue;
        }

        if c.is_ascii_digit() {
            let start = i;
            let mut digits_end = i;
            while digits_end < n && chars[digits_end].1.is_ascii_digit() {
                digits_end += 1;
            }
            // Digits flowing into letters is a numword — `45eagles`, hex
            // runs — kept whole, EXCEPT the scientific tail, which
            // consume_number recognises.
            let end = if digits_end < n
                && at_letter(digits_end)
                && !is_scientific_tail(&chars, digits_end, n)
            {
                digits_end.max(run_end(digits_end))
            } else if i < n
                && digits_end < n
                && chars[digits_end].1 == '/'
                && at_digit(digits_end + 1)
            {
                // The digit/slash join: `07/638`. Consumes further slashes,
                // never dots — the oracle splits `07/638,337` at the comma.
                let mut j = digits_end;
                while j < n && chars[j].1 == '/' && at_digit(j + 1) {
                    j = run_end(j + 1);
                }
                j
            } else {
                consume_number(&chars, i, n)
            };
            push(
                chars[start].0,
                byte_end(end),
                &text[chars[start].0..byte_end(end)],
                &mut position,
            );
            i = end;
            continue;
        }

        if is_word(c) {
            let start = i;
            let end = run_end(i);

            // All-letter hyphen chains compound: `well-established` emits the
            // whole and then its parts. A digit anywhere breaks the chain —
            // `ABC-1234-XY` is a word, a signed number and a word.
            if run_all_letters(start, end) && i_starts_letter_chain(&chars, end, n) {
                let mut parts = vec![(start, end)];
                let mut j = end;
                while j + 1 < n && chars[j].1 == '-' && at_letter(j + 1) {
                    let part_end = run_end(j + 1);
                    if !run_all_letters(j + 1, part_end) {
                        break;
                    }
                    parts.push((j + 1, part_end));
                    j = part_end;
                }
                if parts.len() >= 2 {
                    let chain_end = parts.last().unwrap().1;
                    push(
                        chars[start].0,
                        byte_end(chain_end),
                        &text[chars[start].0..byte_end(chain_end)],
                        &mut position,
                    );
                    for &(a, b) in &parts {
                        push(
                            chars[a].0,
                            byte_end(b),
                            &text[chars[a].0..byte_end(b)],
                            &mut position,
                        );
                    }
                    i = chain_end;
                    continue;
                }
            }

            // A run carrying digits joins across dots to further runs:
            // `v4.2.1` is one token, the oracle's `file` class. A pure-letter
            // run does not — `manifest.json` splits, hosts split, both in the
            // accepted-differences table.
            let mut chain_end = end;
            if run_has_digit(start, end) {
                while chain_end + 1 < n
                    && chars[chain_end].1 == '.'
                    && is_word(chars[chain_end + 1].1)
                {
                    chain_end = run_end(chain_end + 1);
                }
            }
            push(
                chars[start].0,
                byte_end(chain_end),
                &text[chars[start].0..byte_end(chain_end)],
                &mut position,
            );
            i = chain_end;
            continue;
        }

        i += 1;
    }
    tokens
}

/// Whether a hyphen chain of letter parts begins at `i` (which must point at
/// the character after a word run).
fn i_starts_letter_chain(chars: &[(usize, char)], i: usize, n: usize) -> bool {
    i + 1 < n
        && chars[i].1 == '-'
        && chars[i + 1].1.is_alphanumeric()
        && !chars[i + 1].1.is_ascii_digit()
}

/// Consume a number starting at a digit: digits, dotted digit chains, and the
/// scientific tail. Returns the exclusive end index.
fn consume_number(chars: &[(usize, char)], start: usize, n: usize) -> usize {
    let mut i = start;
    while i < n && chars[i].1.is_ascii_digit() {
        i += 1;
    }
    // Dotted chains: one dot is a decimal, more is a version/IP — all one
    // token either way, which is all an index needs.
    while i + 1 < n && chars[i].1 == '.' && chars[i + 1].1.is_ascii_digit() {
        i += 1;
        while i < n && chars[i].1.is_ascii_digit() {
            i += 1;
        }
    }
    // The scientific tail: `e`/`E` + optional sign + digits, not flowing into
    // further letters (`1.2e-9` joins; `12elephants` must not).
    if is_scientific_tail(chars, i, n) {
        i += 1; // e
        if chars[i].1 == '+' || chars[i].1 == '-' {
            i += 1;
        }
        while i < n && chars[i].1.is_ascii_digit() {
            i += 1;
        }
    }
    i
}

fn is_scientific_tail(chars: &[(usize, char)], i: usize, n: usize) -> bool {
    if i >= n || (chars[i].1 != 'e' && chars[i].1 != 'E') {
        return false;
    }
    let mut j = i + 1;
    if j < n && (chars[j].1 == '+' || chars[j].1 == '-') {
        j += 1;
    }
    if j >= n || !chars[j].1.is_ascii_digit() {
        return false;
    }
    while j < n && chars[j].1.is_ascii_digit() {
        j += 1;
    }
    // Digits flowing into letters would make it a word, not an exponent.
    !(j < n && chars[j].1.is_alphanumeric() && !chars[j].1.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(text: &str) -> Vec<String> {
        scan(text).into_iter().map(|t| t.text).collect()
    }

    /// The oracle's own examples, token for token, pre-filters.
    #[test]
    fn the_measured_shapes_tokenize_as_postgresql_does() {
        for (input, want) in [
            ("CVE-2025-31337", vec!["CVE", "-2025", "-31337"]),
            (
                "Part number ABC-1234-XY",
                vec!["Part", "number", "ABC", "-1234", "XY"],
            ),
            ("198.51.100.140", vec!["198.51.100.140"]),
            ("4.2.1", vec!["4.2.1"]),
            ("3.11.7-rc2", vec!["3.11.7", "rc2"]),
            ("v4.2.1", vec!["v4.2.1"]),
            ("$2,520,000.50", vec!["2", "520", "000.50"]),
            (
                "900000.50 but 900000.5",
                vec!["900000.50", "but", "900000.5"],
            ),
            ("-12.75 against +3.5", vec!["-12.75", "against", "+3.5"]),
            (
                "Tolerance 1.2e-9 metres.",
                vec!["Tolerance", "1.2e-9", "metres"],
            ),
            ("07/638,337", vec!["07/638", "337"]),
            (
                "10/123,456 over US7654321B2",
                vec!["10/123", "456", "over", "US7654321B2"],
            ),
            ("0.1%", vec!["0.1"]),
            ("$0.00", vec!["0.00"]),
            ("21 CFR 211.100", vec!["21", "CFR", "211.100"]),
        ] {
            assert_eq!(toks(input), want, "{input:?}");
        }
    }

    /// Hyphenated compounds emit the whole and the parts, in the oracle's
    /// positional order — and only for all-letter chains.
    #[test]
    fn hyphenated_compounds_emit_whole_then_parts() {
        assert_eq!(
            toks("well-established"),
            vec!["well-established", "well", "established"]
        );
        assert_eq!(
            toks("cloud-native multi-tenant"),
            vec![
                "cloud-native",
                "cloud",
                "native",
                "multi-tenant",
                "multi",
                "tenant"
            ]
        );
        // Three parts, one compound.
        assert_eq!(
            toks("state-of-art"),
            vec!["state-of-art", "state", "of", "art"]
        );
        // A digit part breaks the chain instead of compounding.
        assert_eq!(toks("dv-0003"), vec!["dv", "-0003"]);
    }

    /// Positions advance one per emitted token, compounds included — the
    /// layout the oracle's tsvector shows (`'well-establish':1 'well':2 ...`).
    #[test]
    fn positions_count_compounds_and_parts_separately() {
        let tokens = scan("well-established fact");
        let positions: Vec<usize> = tokens.iter().map(|t| t.position).collect();
        assert_eq!(positions, vec![0, 1, 2, 3]);
        assert_eq!(tokens[3].text, "fact");
    }

    /// Byte offsets frame the exact source span, compound and parts alike.
    #[test]
    fn offsets_are_exact_byte_spans() {
        let text = "see CVE-2025-31337 now";
        for t in scan(text) {
            assert_eq!(&text[t.offset_from..t.offset_to], t.text, "{t:?}");
        }
    }

    /// What SimpleTokenizer already got right stays right.
    #[test]
    fn plain_words_are_untouched() {
        assert_eq!(
            toks("The continental congress met in Philadelphia."),
            vec![
                "The",
                "continental",
                "congress",
                "met",
                "in",
                "Philadelphia"
            ]
        );
        assert_eq!(toks(""), Vec::<String>::new());
        assert_eq!(toks("   .,;!  "), Vec::<String>::new());
        assert_eq!(toks("café naïve"), vec!["café", "naïve"]);
    }

    /// Digits flowing into letters stay one word; the scientific tail does
    /// not swallow a word that merely starts with `e`.
    #[test]
    fn numwords_and_near_scientific_shapes() {
        assert_eq!(toks("45eagles"), vec!["45eagles"]);
        assert_eq!(toks("12elephants walked"), vec!["12elephants", "walked"]);
        assert_eq!(toks("2e9"), vec!["2e9"]);
        assert_eq!(toks("1.2e-9"), vec!["1.2e-9"]);
        // The property under test: the exponent is NOT absorbed when its
        // digits flow into letters. The fallout tokens just have to be
        // deterministic; nothing pins them to an oracle row.
        assert_eq!(toks("1.2e-9x"), vec!["1.2", "e", "-9", "x"]);
    }
}
