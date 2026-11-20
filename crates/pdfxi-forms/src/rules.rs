//! Configurable validation rules, formats, custom hooks and deterministic
//! summaries.
//!
//! This module complements the intrinsic checks in [`crate::Field::validate`]
//! with an explicit, caller-configured rule pass. A [`Validator`] associates
//! [`Rule`]s with fully qualified field names and optionally runs
//! [`ValidationHook`] implementations. [`Validator::validate`] returns a
//! [`ValidationSummary`] whose errors are sorted by field name and variant
//! rank, so the result is reproducible.
//!
//! [`Format`] covers numeric, currency, calendar-date and dependency-free
//! [`Pattern`] validation. Patterns implement a deliberately small grammar —
//! literals, `.`, character classes with ranges and negation, the `\\d`, `\\w`,
//! `\\s` shorthand classes, and the `*`, `+`, `?` quantifiers — using a bounded
//! backtracking matcher. No regular-expression crate is used and no input can
//! make the matcher unbounded.

use crate::field::{Field, FieldValue};
use crate::form::Form;
use crate::validation::{ValidationError, ValidationSummary};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum pattern length accepted by [`Pattern::parse`].
pub const MAX_PATTERN_LEN: usize = 512;
/// Maximum text length accepted by the pattern matcher.
pub const MAX_PATTERN_TEXT_LEN: usize = 4096;
/// Backtracking step budget for a single [`Pattern::is_match`] call.
const MATCH_BUDGET: usize = 200_000;

/// An error produced while compiling a [`Pattern`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    /// The pattern exceeded [`MAX_PATTERN_LEN`] characters.
    TooLong(usize),
    /// A character class had no closing `]`.
    UnclosedClass,
    /// A character class contained no members.
    EmptyClass,
    /// A character class range had its bounds in the wrong order.
    InvalidRange(char, char),
    /// A backslash was the final character in the pattern.
    TrailingEscape,
    /// A quantifier appeared with no preceding atom.
    QuantifierWithoutAtom(char),
    /// Two quantifiers were applied to the same atom.
    DoubleQuantifier(char),
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(len) => write!(
                f,
                "pattern has {len} characters (maximum {MAX_PATTERN_LEN})"
            ),
            Self::UnclosedClass => write!(f, "character class is not closed"),
            Self::EmptyClass => write!(f, "character class has no members"),
            Self::InvalidRange(start, end) => {
                write!(f, "invalid character range '{start}-{end}'")
            }
            Self::TrailingEscape => write!(f, "pattern ends with a backslash"),
            Self::QuantifierWithoutAtom(quant) => {
                write!(f, "quantifier '{quant}' has no preceding atom")
            }
            Self::DoubleQuantifier(quant) => {
                write!(f, "quantifier '{quant}' follows another quantifier")
            }
        }
    }
}

impl std::error::Error for PatternError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quantifier {
    One,
    ZeroOrOne,
    ZeroOrMore,
    OneOrMore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    Literal(char),
    Any,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    Digit,
    NotDigit,
    Word,
    NotWord,
    Space,
    NotSpace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    atom: Atom,
    quant: Quantifier,
}

/// A compiled, dependency-free regular-expression-like pattern.
///
/// Full-string matching is used by [`Pattern::is_match`]. A leading `^` and a
/// trailing `$` are accepted for familiarity and ignored because the whole
/// text must match either way. Any other `^` or `$` is treated as a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    source: String,
    tokens: Vec<Token>,
}

impl Pattern {
    /// Compiles `source` into a bounded pattern.
    ///
    /// # Errors
    ///
    /// Returns [`PatternError`] for a pattern longer than
    /// [`MAX_PATTERN_LEN`], an unclosed or empty character class, an inverted
    /// range, a trailing escape, or a misplaced/double quantifier.
    pub fn parse(source: &str) -> Result<Self, PatternError> {
        let chars: Vec<char> = source.chars().collect();
        if chars.len() > MAX_PATTERN_LEN {
            return Err(PatternError::TooLong(chars.len()));
        }
        let mut tokens: Vec<Token> = Vec::with_capacity(chars.len());
        let mut index = 0;
        // Ignore a leading anchor and a trailing anchor: matching is always
        // over the whole string.
        if chars.first() == Some(&'^') {
            index = 1;
        }
        // A trailing '$' is an anchor only when it is not escaped. Counting the
        // immediately preceding backslashes handles '\\$' (escaped literal)
        // and '\\\\$' (escaped backslash followed by an anchor).
        let end = if chars.len() > index && is_unescaped_trailing_anchor(&chars, chars.len() - 1) {
            chars.len() - 1
        } else {
            chars.len()
        };
        while index < end {
            let ch = chars[index];
            match ch {
                '*' | '+' | '?' => {
                    let quant = match ch {
                        '*' => Quantifier::ZeroOrMore,
                        '+' => Quantifier::OneOrMore,
                        _ => Quantifier::ZeroOrOne,
                    };
                    let last = tokens
                        .last_mut()
                        .ok_or(PatternError::QuantifierWithoutAtom(ch))?;
                    if last.quant != Quantifier::One {
                        return Err(PatternError::DoubleQuantifier(ch));
                    }
                    last.quant = quant;
                    index += 1;
                }
                '.' => {
                    push_token(&mut tokens, Atom::Any);
                    index += 1;
                }
                '[' => {
                    let (atom, next) = parse_class(&chars, index, end)?;
                    push_token(&mut tokens, atom);
                    index = next;
                }
                '\\' => {
                    index += 1;
                    if index >= end {
                        return Err(PatternError::TrailingEscape);
                    }
                    let atom = match chars[index] {
                        'd' => Atom::Digit,
                        'D' => Atom::NotDigit,
                        'w' => Atom::Word,
                        'W' => Atom::NotWord,
                        's' => Atom::Space,
                        'S' => Atom::NotSpace,
                        other => Atom::Literal(other),
                    };
                    push_token(&mut tokens, atom);
                    index += 1;
                }
                other => {
                    push_token(&mut tokens, Atom::Literal(other));
                    index += 1;
                }
            }
        }
        Ok(Self {
            source: source.to_owned(),
            tokens,
        })
    }

    /// Returns the original pattern source.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns `true` when the entire `text` matches this pattern.
    ///
    /// Input longer than [`MAX_PATTERN_TEXT_LEN`] is rejected without
    /// matching. Backtracking is bounded by a fixed step budget, so a pattern
    /// such as `a*a*a*` cannot make this function run unbounded.
    pub fn is_match(&self, text: &str) -> bool {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() > MAX_PATTERN_TEXT_LEN {
            return false;
        }
        let mut budget = MATCH_BUDGET;
        matches_from(&self.tokens, 0, &chars, 0, &mut budget)
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

/// Returns `true` when `chars[index]` is a `$` preceded by an even number of
/// backslashes, i.e. an unescaped trailing anchor rather than a literal.
fn is_unescaped_trailing_anchor(chars: &[char], index: usize) -> bool {
    if chars.get(index) != Some(&'$') {
        return false;
    }
    let mut cursor = index;
    let mut backslashes = 0usize;
    while cursor > 0 && chars[cursor - 1] == '\\' {
        backslashes += 1;
        cursor -= 1;
    }
    backslashes.is_multiple_of(2)
}

fn push_token(tokens: &mut Vec<Token>, atom: Atom) {
    tokens.push(Token {
        atom,
        quant: Quantifier::One,
    });
}

/// Parses a `[...]` class starting at `start`, returning the atom and the
/// index just past the closing bracket.
fn parse_class(chars: &[char], start: usize, end: usize) -> Result<(Atom, usize), PatternError> {
    let mut index = start + 1;
    let negated = chars.get(index) == Some(&'^');
    if negated {
        index += 1;
    }
    let mut ranges: Vec<(char, char)> = Vec::new();
    while index < end && chars[index] != ']' {
        let first = class_char(chars, &mut index, end)?;
        if chars.get(index) == Some(&'-') && index + 1 < end && chars[index + 1] != ']' {
            index += 1;
            let last = class_char(chars, &mut index, end)?;
            if first > last {
                return Err(PatternError::InvalidRange(first, last));
            }
            ranges.push((first, last));
        } else {
            ranges.push((first, first));
        }
    }
    if index >= end {
        return Err(PatternError::UnclosedClass);
    }
    if ranges.is_empty() {
        return Err(PatternError::EmptyClass);
    }
    Ok((Atom::Class { negated, ranges }, index + 1))
}

/// Reads one class member, honouring backslash escapes.
fn class_char(chars: &[char], index: &mut usize, end: usize) -> Result<char, PatternError> {
    if *index >= end {
        return Err(PatternError::UnclosedClass);
    }
    if chars[*index] == '\\' {
        *index += 1;
        if *index >= end {
            return Err(PatternError::TrailingEscape);
        }
    }
    let ch = chars[*index];
    *index += 1;
    Ok(ch)
}

fn atom_matches(atom: &Atom, ch: char) -> bool {
    match atom {
        Atom::Literal(expected) => *expected == ch,
        Atom::Any => ch != '\n' && ch != '\r',
        Atom::Class { negated, ranges } => {
            let inside = ranges.iter().any(|(start, end)| *start <= ch && ch <= *end);
            inside != *negated
        }
        Atom::Digit => ch.is_ascii_digit(),
        Atom::NotDigit => !ch.is_ascii_digit(),
        Atom::Word => ch.is_ascii_alphanumeric() || ch == '_',
        Atom::NotWord => !(ch.is_ascii_alphanumeric() || ch == '_'),
        Atom::Space => ch.is_whitespace(),
        Atom::NotSpace => !ch.is_whitespace(),
    }
}

fn matches_from(
    tokens: &[Token],
    token_index: usize,
    chars: &[char],
    pos: usize,
    budget: &mut usize,
) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    if token_index == tokens.len() {
        return pos == chars.len();
    }
    let token = &tokens[token_index];
    match token.quant {
        Quantifier::One => {
            pos < chars.len()
                && atom_matches(&token.atom, chars[pos])
                && matches_from(tokens, token_index + 1, chars, pos + 1, budget)
        }
        Quantifier::ZeroOrOne => {
            if pos < chars.len()
                && atom_matches(&token.atom, chars[pos])
                && matches_from(tokens, token_index + 1, chars, pos + 1, budget)
            {
                return true;
            }
            matches_from(tokens, token_index + 1, chars, pos, budget)
        }
        Quantifier::ZeroOrMore | Quantifier::OneOrMore => {
            let mut end = pos;
            while end < chars.len() && atom_matches(&token.atom, chars[end]) {
                end += 1;
            }
            if token.quant == Quantifier::OneOrMore && end == pos {
                return false;
            }
            for candidate in (pos..=end).rev() {
                if matches_from(tokens, token_index + 1, chars, candidate, budget) {
                    return true;
                }
            }
            false
        }
    }
}

/// Currency formatting options used by [`Format::Currency`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurrencyFormat {
    symbol: Option<char>,
    grouping: bool,
    decimals: u8,
}

impl CurrencyFormat {
    /// Creates a currency format.
    ///
    /// `decimals` is clamped to `0..=6`.
    pub const fn new(symbol: Option<char>, grouping: bool, decimals: u8) -> Self {
        Self {
            symbol,
            grouping,
            decimals: if decimals > 6 { 6 } else { decimals },
        }
    }

    /// A plain amount: no symbol, no grouping, exactly two decimals.
    pub const fn plain() -> Self {
        Self::new(None, false, 2)
    }

    /// A US-dollar amount: `$` symbol, grouping, exactly two decimals.
    pub const fn usd() -> Self {
        Self::new(Some('$'), true, 2)
    }

    /// Returns the optional currency symbol.
    pub const fn symbol(self) -> Option<char> {
        self.symbol
    }

    /// Returns whether thousands grouping separators are allowed.
    pub const fn grouping(self) -> bool {
        self.grouping
    }

    /// Returns the exact number of fractional digits required.
    pub const fn decimals(self) -> u8 {
        self.decimals
    }
}

/// Calendar date layouts accepted by [`Format::Date`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateLayout {
    /// `YYYY-MM-DD` (ISO 8601).
    Iso,
    /// `MM/DD/YYYY` (US).
    Us,
    /// `DD.MM.YYYY` (European).
    European,
}

impl DateLayout {
    /// Returns a stable human-readable label for this layout.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Iso => "date (YYYY-MM-DD)",
            Self::Us => "date (MM/DD/YYYY)",
            Self::European => "date (DD.MM.YYYY)",
        }
    }
}

/// A named value format checked by [`Rule::Format`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Format {
    /// A finite decimal number, with optional sign and exponent.
    Numeric,
    /// A currency amount with optional symbol and grouping.
    Currency(CurrencyFormat),
    /// A calendar date in a fixed layout.
    Date(DateLayout),
    /// A dependency-free [`Pattern`].
    Pattern(Pattern),
}

impl Format {
    /// Returns a stable human-readable label for this format.
    pub fn label(&self) -> String {
        match self {
            Self::Numeric => "numeric".to_owned(),
            Self::Currency(_) => "currency".to_owned(),
            Self::Date(layout) => layout.label().to_owned(),
            Self::Pattern(pattern) => format!("pattern '{pattern}'"),
        }
    }

    pub(crate) fn check(&self, text: &str) -> Result<(), String> {
        match self {
            Self::Numeric => check_numeric(text),
            Self::Currency(format) => check_currency(text, *format),
            Self::Date(layout) => check_date(text, *layout),
            Self::Pattern(pattern) => {
                if pattern.is_match(text) {
                    Ok(())
                } else {
                    Err(format!("text does not match pattern '{pattern}'"))
                }
            }
        }
    }
}

/// A single validation rule attached to a field name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// The field must carry a non-empty value.
    Required,
    /// A text value must not exceed the given number of characters.
    MaxLength(usize),
    /// A choice or radio selection must be one of the field's options.
    ChoiceMembership,
    /// The value must satisfy the given [`Format`].
    Format(Format),
}

/// A user-supplied validation callback.
///
/// Hooks run for every field in the form in stable field order. They must be
/// side-effect free and return `Ok(())` for fields they do not care about.
pub trait ValidationHook {
    /// Validates one field, returning a rejection reason on failure.
    fn validate_field(&self, field: &Field) -> Result<(), String>;
}

impl<F> ValidationHook for F
where
    F: Fn(&Field) -> Result<(), String>,
{
    fn validate_field(&self, field: &Field) -> Result<(), String> {
        self(field)
    }
}

/// A rule-based validator producing deterministic summaries.
///
/// Create one with [`Validator::new`], add rules with [`Validator::with_rule`]
/// and hooks with [`Validator::with_hook`], then call [`Validator::validate`].
#[derive(Default)]
pub struct Validator<'a> {
    rules: BTreeMap<String, Vec<Rule>>,
    hooks: Vec<&'a dyn ValidationHook>,
}

impl<'a> Validator<'a> {
    /// Creates an empty validator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a rule to a fully qualified field name.
    #[must_use]
    pub fn with_rule(mut self, field: impl Into<String>, rule: Rule) -> Self {
        self.rules.entry(field.into()).or_default().push(rule);
        self
    }

    /// Registers a custom validation hook.
    #[must_use]
    pub fn with_hook(mut self, hook: &'a dyn ValidationHook) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Returns the rule count (including rules for unknown fields).
    pub fn rule_count(&self) -> usize {
        self.rules.values().map(Vec::len).sum()
    }

    /// Validates `form`, returning a deterministic [`ValidationSummary`].
    ///
    /// Rules for names that are absent from the form are reported as
    /// [`ValidationError::UnknownField`] rather than silently ignored.
    pub fn validate(&self, form: &Form) -> ValidationSummary {
        let mut errors = Vec::new();
        for name in self.rules.keys() {
            if form.field(name).is_none() {
                errors.push(ValidationError::UnknownField {
                    field: name.clone(),
                });
            }
        }
        for field in form.fields() {
            if let Some(rules) = self.rules.get(field.name()) {
                for rule in rules {
                    check_rule(rule, field, &mut errors);
                }
            }
            for hook in &self.hooks {
                if let Err(reason) = hook.validate_field(field) {
                    errors.push(ValidationError::CustomRejected {
                        field: field.name().to_owned(),
                        reason,
                    });
                }
            }
        }
        ValidationSummary::new(form.len(), errors)
    }
}

fn check_rule(rule: &Rule, field: &Field, errors: &mut Vec<ValidationError>) {
    let name = field.name();
    let value = field.value();
    match rule {
        Rule::Required => {
            let empty = value.is_empty()
                || matches!(value, FieldValue::Choice(values) if values.is_empty());
            if empty {
                errors.push(ValidationError::MissingRequired {
                    field: name.to_owned(),
                });
            }
        }
        Rule::MaxLength(max) => {
            if let Some(text) = value.as_text() {
                let actual = text.chars().count();
                if actual > *max {
                    errors.push(ValidationError::MaxLengthExceeded {
                        field: name.to_owned(),
                        max: *max,
                        actual,
                    });
                }
            }
        }
        Rule::ChoiceMembership => {
            if matches!(
                field.field_type(),
                crate::field::FieldType::Choice | crate::field::FieldType::Radio
            ) && let Some(selected) = value.as_choices()
            {
                for item in selected {
                    if !field.choices().iter().any(|option| option == item) {
                        errors.push(ValidationError::InvalidChoice {
                            field: name.to_owned(),
                            value: item.clone(),
                        });
                    }
                }
            }
        }
        Rule::Format(format) => {
            let text = match value {
                FieldValue::Text(text) => Some(text.clone()),
                FieldValue::Number(number) if number.is_finite() => {
                    if let Format::Currency(currency) = format {
                        let prec = usize::from(currency.decimals());
                        Some(format!("{number:.prec$}"))
                    } else {
                        Some(format!("{number}"))
                    }
                }
                _ => None,
            };
            let Some(text) = text else {
                errors.push(ValidationError::InvalidFormat {
                    field: name.to_owned(),
                    format: format.label(),
                    detail: "value is not text or a finite number".to_owned(),
                });
                return;
            };
            if let Err(detail) = format.check(&text) {
                errors.push(ValidationError::InvalidFormat {
                    field: name.to_owned(),
                    format: format.label(),
                    detail,
                });
            }
        }
    }
}

fn check_numeric(text: &str) -> Result<(), String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("empty text is not a number".to_owned());
    }
    match trimmed.parse::<f64>() {
        Ok(number) if number.is_finite() => Ok(()),
        Ok(_) => Err("number is not finite".to_owned()),
        Err(_) => Err(format!("'{text}' is not a number")),
    }
}

fn check_currency(text: &str, format: CurrencyFormat) -> Result<(), String> {
    let mut rest = text.trim();
    if let Some(symbol) = format.symbol() {
        if !rest.starts_with(symbol) {
            return Err(format!("missing currency symbol '{symbol}'"));
        }
        rest = &rest[symbol.len_utf8()..];
    }
    if let Some(stripped) = rest.strip_prefix(|c| c == '+' || c == '-') {
        rest = stripped;
    }
    let (integer, fraction) = match rest.split_once('.') {
        Some((left, right)) => (left, Some(right)),
        None => (rest, None),
    };
    if integer.is_empty() {
        return Err("currency amount has no integer part".to_owned());
    }
    let digits = format.decimals() as usize;
    match fraction {
        Some(fraction) => {
            if fraction.chars().count() != digits || !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("expected exactly {digits} fractional digits"));
            }
        }
        None if digits != 0 => return Err(format!("expected exactly {digits} fractional digits")),
        None => {}
    }
    if format.grouping() {
        check_grouped_integer(integer)?;
    } else if !integer.bytes().all(|b| b.is_ascii_digit()) {
        return Err("currency amount contains non-digit characters".to_owned());
    }
    Ok(())
}

fn check_grouped_integer(integer: &str) -> Result<(), String> {
    let groups: Vec<&str> = integer.split(',').collect();
    if groups.is_empty() || groups[0].is_empty() || groups[0].len() > 3 {
        return Err("invalid thousands grouping".to_owned());
    }
    if !groups[0].bytes().all(|b| b.is_ascii_digit()) {
        return Err("invalid thousands grouping".to_owned());
    }
    for group in groups.iter().skip(1) {
        if group.len() != 3 || !group.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid thousands grouping".to_owned());
        }
    }
    Ok(())
}

fn check_date(text: &str, layout: DateLayout) -> Result<(), String> {
    let (separator, order) = match layout {
        DateLayout::Iso => ('-', [0usize, 1, 2]),
        DateLayout::Us => ('/', [2usize, 0, 1]),
        DateLayout::European => ('.', [2usize, 1, 0]),
    };
    let parts: Vec<&str> = text.split(separator).collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected three components separated by '{separator}'"
        ));
    }
    let parse = |index: usize, width: usize| -> Option<u32> {
        let part = parts[index];
        if part.len() == width && part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse::<u32>().ok()
        } else {
            None
        }
    };
    let [y, m, d] = order;
    let (year, month, day) = (parse(y, 4), parse(m, 2), parse(d, 2));
    let (year, month, day) = match (year, month, day) {
        (Some(y), Some(m), Some(d)) => (y, m, d),
        _ => return Err("components have the wrong width or are not digits".to_owned()),
    };
    if !(1..=12).contains(&month) {
        return Err("month is out of range".to_owned());
    }
    let max_day = days_in_month(year, month);
    if day == 0 || day > max_day {
        return Err(format!("day is out of range for month {month}"));
    }
    Ok(())
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{FieldFlags, FieldType};
    use crate::form::Form;

    fn form() -> Form {
        Form::from_fields([
            Field::new("name", FieldType::Text).with_flags(FieldFlags::REQUIRED),
            Field::new("age", FieldType::Text),
            Field::new("color", FieldType::Choice).with_choices(["red", "green"]),
        ])
        .unwrap()
    }

    #[test]
    fn pattern_matches_literals_classes_and_quantifiers() {
        let digits = Pattern::parse(r"\d\d\d").unwrap();
        assert!(digits.is_match("123"));
        assert!(!digits.is_match("12a"));
        let word = Pattern::parse(r"[A-Za-z_]\w*").unwrap();
        assert!(word.is_match("Field_1"));
        assert!(!word.is_match("1field"));
        let negated = Pattern::parse("[^0-9]+").unwrap();
        assert!(negated.is_match("abc"));
        assert!(!negated.is_match("a1b"));
        let optional = Pattern::parse("colou?r").unwrap();
        assert!(optional.is_match("color"));
        assert!(optional.is_match("colour"));
        assert!(!optional.is_match("colouur"));
    }

    #[test]
    fn pattern_rejects_malformed_sources() {
        assert_eq!(
            Pattern::parse("[abc").unwrap_err(),
            PatternError::UnclosedClass
        );
        assert_eq!(Pattern::parse("[]").unwrap_err(), PatternError::EmptyClass);
        assert_eq!(
            Pattern::parse("[z-a]").unwrap_err(),
            PatternError::InvalidRange('z', 'a')
        );
        assert_eq!(
            Pattern::parse("abc\\").unwrap_err(),
            PatternError::TrailingEscape
        );
        assert_eq!(
            Pattern::parse("*a").unwrap_err(),
            PatternError::QuantifierWithoutAtom('*')
        );
        assert_eq!(
            Pattern::parse("a**").unwrap_err(),
            PatternError::DoubleQuantifier('*')
        );
    }

    #[test]
    fn escaped_trailing_dollar_is_a_literal() {
        let literal = Pattern::parse(r"x\$").unwrap();
        assert!(literal.is_match("x$"));
        assert!(!literal.is_match("x"));
        assert_eq!(literal.source(), r"x\$");

        // An unescaped trailing '$' remains a no-op full-match anchor.
        let anchored = Pattern::parse("x$").unwrap();
        assert!(anchored.is_match("x"));
        assert!(!anchored.is_match("xy"));

        // An escaped backslash before '$' leaves a genuine anchor.
        let escaped_slash = Pattern::parse(r"x\\$").unwrap();
        assert!(escaped_slash.is_match(r"x\"));
        assert!(!escaped_slash.is_match(r"x\$"));
    }

    #[test]
    fn numeric_currency_and_date_formats_are_checked() {
        assert!(Format::Numeric.check(" -12.5e2 ").is_ok());
        assert!(Format::Numeric.check("abc").is_err());
        assert!(
            Format::Currency(CurrencyFormat::usd())
                .check("$1,234.56")
                .is_ok()
        );
        assert!(
            Format::Currency(CurrencyFormat::plain())
                .check("12.5")
                .is_err()
        );
        assert!(Format::Date(DateLayout::Iso).check("2024-02-29").is_ok());
        assert!(Format::Date(DateLayout::Iso).check("2023-02-29").is_err());
        assert!(Format::Date(DateLayout::Us).check("12/31/2024").is_ok());
        assert!(
            Format::Date(DateLayout::European)
                .check("29.02.2024")
                .is_ok()
        );
    }

    #[test]
    fn validator_checks_required_max_length_and_choice_membership() {
        let mut form = form();
        form.set_value("name", FieldValue::Text("Ada".into()))
            .unwrap();
        form.set_value("age", FieldValue::Text("123456".into()))
            .unwrap();
        form.set_value("color", FieldValue::Choice(vec!["purple".into()]))
            .unwrap();
        let validator = Validator::new()
            .with_rule("name", Rule::Required)
            .with_rule("age", Rule::MaxLength(5))
            .with_rule("color", Rule::ChoiceMembership);
        let summary = validator.validate(&form);

        assert_eq!(summary.error_count(), 2);
        assert!(matches!(
            &summary.errors()[0],
            ValidationError::MaxLengthExceeded { field, max, actual }
                if field == "age" && *max == 5 && *actual == 6
        ));
        assert!(matches!(
            &summary.errors()[1],
            ValidationError::InvalidChoice { field, value } if field == "color" && value == "purple"
        ));
    }

    #[test]
    fn required_rule_reports_empty_and_empty_choice_lists() {
        let form = Form::from_fields([
            Field::new("text", FieldType::Text),
            Field::new("choice", FieldType::Choice)
                .with_choices(["a"])
                .with_value(FieldValue::Choice(Vec::new())),
        ])
        .unwrap();
        let validator = Validator::new()
            .with_rule("text", Rule::Required)
            .with_rule("choice", Rule::Required);
        let summary = validator.validate(&form);

        assert_eq!(summary.invalid_field_names(), ["choice", "text"]);
    }

    #[test]
    fn format_rule_fails_on_numeric_and_date_inputs() {
        let form = Form::from_fields([
            Field::new("bad", FieldType::Text).with_value(FieldValue::Text("abc".into())),
            Field::new("good", FieldType::Text).with_value(FieldValue::Text("42".into())),
        ])
        .unwrap();
        let validator = Validator::new()
            .with_rule("bad", Rule::Format(Format::Numeric))
            .with_rule("good", Rule::Format(Format::Numeric));
        let summary = validator.validate(&form);

        assert_eq!(summary.error_count(), 1);
        assert!(matches!(
            &summary.errors()[0],
            ValidationError::InvalidFormat { field, format, .. }
                if field == "bad" && format == "numeric"
        ));
    }

    #[test]
    fn custom_hook_can_reject_a_field() {
        let hook = |field: &Field| {
            if field.name() == "name" && field.value().as_text() == Some("bad") {
                Err("reserved name".to_owned())
            } else {
                Ok(())
            }
        };
        let form = Form::from_fields([
            Field::new("name", FieldType::Text).with_value(FieldValue::Text("bad".into())),
            Field::new("other", FieldType::Text),
        ])
        .unwrap();
        let summary = Validator::new().with_hook(&hook).validate(&form);

        assert_eq!(summary.error_count(), 1);
        assert!(matches!(
            &summary.errors()[0],
            ValidationError::CustomRejected { field, reason }
                if field == "name" && reason == "reserved name"
        ));
    }

    #[test]
    fn summary_is_deterministic_and_reports_unknown_fields() {
        let form = Form::from_fields([
            Field::new("zeta", FieldType::Text).with_value(FieldValue::Text("x".into())),
            Field::new("alpha", FieldType::Text).with_value(FieldValue::Text("x".into())),
        ])
        .unwrap();
        let validator = Validator::new()
            .with_rule("zeta", Rule::MaxLength(0))
            .with_rule("alpha", Rule::MaxLength(0))
            .with_rule("ghost", Rule::Required);
        let first = validator.validate(&form);
        let second = validator.validate(&form);

        assert_eq!(first, second);
        assert_eq!(first.summary_line(), "3 error(s) across 3 of 2 field(s)");
        assert_eq!(first.invalid_field_names(), ["alpha", "ghost", "zeta"]);
        assert!(matches!(
            &first.errors()[0],
            ValidationError::MaxLengthExceeded { field, .. } if field == "alpha"
        ));
        assert!(matches!(
            &first.errors()[1],
            ValidationError::UnknownField { field } if field == "ghost"
        ));
    }
}
