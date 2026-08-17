//! Strict field grammars for order-book cells.
//!
//! Fail-skip contract: every parser either returns an exact value or a typed
//! rejection. There is no repair beyond *presentation folding* (full-width
//! forms and the CJK colon fold to their ASCII equivalents); `O`→`0`-style
//! guessing is banned.

use ptt_core::Decimal;

/// Comparator prefix on a boundary row (`<1:9.87`, `>1:9.75`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparator {
    Exact,
    LessThan,
    GreaterThan,
}

impl Comparator {
    pub fn as_str(self) -> &'static str {
        match self {
            Comparator::Exact => "",
            Comparator::LessThan => "<",
            Comparator::GreaterThan => ">",
        }
    }
}

/// An exactly-parsed ratio cell: `left:right` in on-screen order (left is the
/// "I need" unit count, right the "I have" unit count). Semantics are assigned
/// at book-assembly level; this layer only guarantees exact digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatioField {
    pub comparator: Comparator,
    pub left: Decimal,
    pub right: Decimal,
    /// Normalized text the values were parsed from, for provenance.
    pub normalized: String,
}

/// Typed rejection reasons. `NeedsReview`-style softening happens upstream;
/// the grammar itself is binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldReject {
    Empty,
    /// Character outside the closed set for this field.
    ForbiddenCharacter(char),
    /// Structure broken: missing/duplicate colon, dangling comparator, etc.
    Malformed,
    /// Comma grouping present but not strict 3-digit groups.
    CommaGrouping,
    /// More than two fractional digits, or a bare/trailing decimal point.
    FractionShape,
    /// Zero, negative, or beyond the plausible on-screen range.
    OutOfRange,
}

/// Presentation folding shared by all field parsers: trims, folds full-width
/// digits/punctuation to ASCII, folds the CJK colon. Never touches letter
/// shapes and never maps letters to digits.
fn fold_presentation(text: &str) -> String {
    text.trim()
        .chars()
        .filter_map(|character| match character {
            '\u{FF10}'..='\u{FF19}' => {
                char::from_u32(u32::from(character) - 0xFF10 + u32::from('0'))
            }
            // Full-width and ratio colons, plus the bullet/middle-dot shapes
            // Windows OCR emits for the game's small ratio colon glyph.
            '\u{FF1A}' | '\u{2236}' | '\u{2022}' | '\u{00B7}' | ';' => Some(':'),
            '\u{FF0C}' => Some(','),
            '\u{FF0E}' | '\u{2024}' => Some('.'),
            '\u{FF1C}' => Some('<'),
            '\u{FF1E}' => Some('>'),
            '\u{00A0}' | '\u{2009}' | '\u{200A}' => Some(' '),
            other => Some(other),
        })
        .collect()
}

/// Parses one numeric side of a ratio: optional strict comma grouping,
/// optional 1–2 digit fraction. Returns the exact decimal.
fn parse_side(raw: &str) -> Result<Decimal, FieldReject> {
    if raw.is_empty() {
        return Err(FieldReject::Empty);
    }
    for character in raw.chars() {
        if !character.is_ascii_digit() && character != ',' && character != '.' {
            return Err(FieldReject::ForbiddenCharacter(character));
        }
    }
    let (integer_part, fraction_part) = match raw.split_once('.') {
        None => (raw, None),
        Some((integer, fraction)) => {
            if fraction.is_empty()
                || fraction.len() > 2
                || !fraction.chars().all(|c| c.is_ascii_digit())
            {
                return Err(FieldReject::FractionShape);
            }
            (integer, Some(fraction))
        }
    };
    validate_integer_grouping(integer_part)?;
    let digits: String = integer_part.chars().filter(|c| *c != ',').collect();
    let joined = match fraction_part {
        Some(fraction) => format!("{digits}.{fraction}"),
        None => digits,
    };
    let value: Decimal = joined
        .parse::<Decimal>()
        .map_err(|_| FieldReject::Malformed)?;
    if value <= Decimal::from(0) {
        return Err(FieldReject::OutOfRange);
    }
    Ok(value)
}

/// Integer part: plain digits, or strict 3-digit comma groups (`1,823,312`).
fn validate_integer_grouping(integer: &str) -> Result<(), FieldReject> {
    if integer.is_empty() {
        return Err(FieldReject::Malformed);
    }
    if !integer.contains(',') {
        return Ok(());
    }
    let groups: Vec<&str> = integer.split(',').collect();
    let head_ok = !groups[0].is_empty() && groups[0].len() <= 3;
    if !head_ok || groups[1..].iter().any(|group| group.len() != 3) {
        return Err(FieldReject::CommaGrouping);
    }
    Ok(())
}

const MAXIMUM_RATIO_SIDE: u64 = 1_000_000;

/// Parses a full ratio cell: optional `<`/`>` prefix, then `NUM:NUM`.
pub fn parse_ratio(text: &str) -> Result<RatioField, FieldReject> {
    let folded = fold_presentation(text);
    let trimmed = folded.trim();
    if trimmed.is_empty() {
        return Err(FieldReject::Empty);
    }
    let (comparator, body) = match trimmed.strip_prefix('<') {
        Some(rest) => (Comparator::LessThan, rest.trim_start()),
        None => match trimmed.strip_prefix('>') {
            Some(rest) => (Comparator::GreaterThan, rest.trim_start()),
            None => (Comparator::Exact, trimmed),
        },
    };
    // Internal whitespace is a glue symptom (a wrapped fragment abutting the
    // next row), never a valid ratio shape — reject instead of compacting.
    if body.contains(char::is_whitespace) || body.contains('<') || body.contains('>') {
        return Err(FieldReject::Malformed);
    }
    let Some((left_raw, right_raw)) = body.split_once(':') else {
        return Err(FieldReject::Malformed);
    };
    if right_raw.contains(':') {
        return Err(FieldReject::Malformed);
    }
    let left = parse_side(left_raw)?;
    let right = parse_side(right_raw)?;
    let ceiling = Decimal::from(MAXIMUM_RATIO_SIDE);
    if left > ceiling || right > ceiling {
        return Err(FieldReject::OutOfRange);
    }
    Ok(RatioField {
        comparator,
        left,
        right,
        normalized: format!("{}{left_raw}:{right_raw}", comparator.as_str()),
    })
}

const MAXIMUM_STOCK: u64 = 100_000_000;

/// Parses a stock cell: positive integer, optional strict comma grouping.
pub fn parse_stock(text: &str) -> Result<u64, FieldReject> {
    let folded = fold_presentation(text);
    let compact: String = folded.chars().filter(|c| *c != ' ').collect();
    if compact.is_empty() {
        return Err(FieldReject::Empty);
    }
    for character in compact.chars() {
        if !character.is_ascii_digit() && character != ',' {
            return Err(FieldReject::ForbiddenCharacter(character));
        }
    }
    validate_integer_grouping(&compact)?;
    // Stock is always rendered with thousands separators — every one of the
    // fourteen four-plus-digit stocks across both corpora carries them, and
    // none lacks them. So a long run of bare digits is not a big number, it is
    // a misread separator: `82,000` came back as `821000`, which parses,
    // validates, and is wrong by a factor of ten. Rejecting it here routes the
    // row through the PP-OCRv5 retry, which reads the comma.
    //
    // Ratios are exempt and must stay that way: the panel prints those without
    // separators, `1 : 3440` among them.
    let digit_count = compact.chars().filter(char::is_ascii_digit).count();
    if digit_count >= 4 && !compact.contains(',') {
        return Err(FieldReject::CommaGrouping);
    }
    let digits: String = compact.chars().filter(|c| *c != ',').collect();
    let value: u64 = digits.parse().map_err(|_| FieldReject::Malformed)?;
    if value == 0 || value > MAXIMUM_STOCK {
        return Err(FieldReject::OutOfRange);
    }
    Ok(value)
}

/// Splits one recognized row line into (ratio text, stock text) by taking the
/// last whitespace-separated token as stock. Glued digits (no separator) are
/// rejected rather than guessed apart.
pub fn split_row_line(line: &str) -> Result<(String, String), FieldReject> {
    let folded = fold_presentation(line);
    let tokens: Vec<&str> = folded.split_whitespace().collect();
    match tokens.as_slice() {
        [] => Err(FieldReject::Empty),
        [_single] => Err(FieldReject::Malformed),
        [ratio_tokens @ .., stock] => Ok((ratio_tokens.concat(), (*stock).to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimal(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    #[test]
    fn plain_and_decimal_ratios_parse_exactly() {
        let ratio = parse_ratio("1:9.80").unwrap();
        assert_eq!(ratio.comparator, Comparator::Exact);
        assert_eq!(ratio.left, decimal("1"));
        assert_eq!(ratio.right, decimal("9.8"));

        let wrapped = parse_ratio("1:332.46").unwrap();
        assert_eq!(wrapped.right, decimal("332.46"));

        let grouped = parse_ratio("1,000.50:2").unwrap();
        assert_eq!(grouped.left, decimal("1000.5"));
    }

    #[test]
    fn comparator_prefixes_are_recognized() {
        assert_eq!(
            parse_ratio("<1:9.87").unwrap().comparator,
            Comparator::LessThan
        );
        assert_eq!(
            parse_ratio("> 1:9.75").unwrap().comparator,
            Comparator::GreaterThan
        );
        assert_eq!(
            parse_ratio("＜1:359").unwrap().comparator,
            Comparator::LessThan
        );
    }

    #[test]
    fn malformed_ratios_are_rejected_not_repaired() {
        assert!(matches!(parse_ratio(""), Err(FieldReject::Empty)));
        assert!(matches!(parse_ratio("1:9:80"), Err(FieldReject::Malformed)));
        assert!(matches!(parse_ratio("19.80"), Err(FieldReject::Malformed)));
        assert!(matches!(
            parse_ratio("1:9.804"),
            Err(FieldReject::FractionShape)
        ));
        assert!(matches!(
            parse_ratio("1:9."),
            Err(FieldReject::FractionShape)
        ));
        assert!(matches!(
            parse_ratio("l:9.80"),
            Err(FieldReject::ForbiddenCharacter('l'))
        ));
        assert!(matches!(parse_ratio("0:5"), Err(FieldReject::OutOfRange)));
        assert!(matches!(
            parse_ratio("1:12,34"),
            Err(FieldReject::CommaGrouping)
        ));
        assert!(matches!(parse_ratio("<>1:2"), Err(FieldReject::Malformed)));
    }

    #[test]
    fn stock_accepts_strict_grouping_only() {
        assert_eq!(parse_stock("920").unwrap(), 920);
        assert_eq!(parse_stock("1,823,312").unwrap(), 1_823_312);
        assert_eq!(parse_stock("23,902").unwrap(), 23_902);
        assert!(matches!(
            parse_stock("1,82,312"),
            Err(FieldReject::CommaGrouping)
        ));
        assert!(matches!(parse_stock("0"), Err(FieldReject::OutOfRange)));
        assert!(matches!(
            parse_stock("9O2"),
            Err(FieldReject::ForbiddenCharacter('O'))
        ));
        assert!(matches!(parse_stock(""), Err(FieldReject::Empty)));
    }

    #[test]
    fn full_width_presentation_folds_but_letters_never_do() {
        assert_eq!(parse_stock("１，８２３，３１２").unwrap(), 1_823_312);
        let ratio = parse_ratio("１：９.８０").unwrap();
        assert_eq!(ratio.right, decimal("9.8"));
    }

    #[test]
    fn row_line_splits_on_last_token() {
        assert_eq!(
            split_row_line("1:9.80 920").unwrap(),
            ("1:9.80".to_string(), "920".to_string())
        );
        assert_eq!(
            split_row_line("< 1:9.87 23,902").unwrap(),
            ("<1:9.87".to_string(), "23,902".to_string())
        );
        assert!(matches!(
            split_row_line("1:9.80920"),
            Err(FieldReject::Malformed)
        ));
        assert!(matches!(split_row_line("  "), Err(FieldReject::Empty)));
    }

    #[test]
    fn glued_wrap_digits_fail_ratio_grammar() {
        // A wrapped second line glued onto the next row's digits must never
        // parse as a plausible ratio.
        assert!(parse_ratio("46 1:333").is_err());
        assert!(parse_ratio(".461:333").is_err());
    }
}
