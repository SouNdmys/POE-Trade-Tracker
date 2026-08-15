use std::cmp::Ordering;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct MarketAssetId(String);

impl<'de> Deserialize<'de> for MarketAssetId {
    fn deserialize<DeserializerType>(
        deserializer: DeserializerType,
    ) -> Result<Self, DeserializerType::Error>
    where
        DeserializerType: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(DeserializerType::Error::custom)
    }
}

impl MarketAssetId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 96
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid || value.starts_with('-') || value.ends_with('-') || value.contains("--") {
            return Err(DomainError::InvalidMarketAssetId);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MarketAssetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Game {
    Poe1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Realm {
    Pc,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientLanguage {
    English,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationIdentity {
    pub ocr_provider_id: String,
    pub ocr_provider_version: String,
    pub ocr_model_sha256: String,
    pub ocr_provider_manifest_sha256: String,
    pub parser_assets_sha256: String,
    pub product_catalog_version: String,
    pub product_catalog_sha256: String,
    pub recognition_catalog_version: String,
    pub recognition_catalog_sha256: String,
    pub recognition_template_version: String,
}

impl ObservationIdentity {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        ocr_provider_id: impl Into<String>,
        ocr_provider_version: impl Into<String>,
        ocr_model_sha256: impl Into<String>,
        ocr_provider_manifest_sha256: impl Into<String>,
        parser_assets_sha256: impl Into<String>,
        product_catalog_version: impl Into<String>,
        product_catalog_sha256: impl Into<String>,
        recognition_catalog_version: impl Into<String>,
        recognition_catalog_sha256: impl Into<String>,
        recognition_template_version: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let identity = Self {
            ocr_provider_id: ocr_provider_id.into().trim().to_owned(),
            ocr_provider_version: ocr_provider_version.into().trim().to_owned(),
            ocr_model_sha256: ocr_model_sha256.into().trim().to_owned(),
            ocr_provider_manifest_sha256: ocr_provider_manifest_sha256.into().trim().to_owned(),
            parser_assets_sha256: parser_assets_sha256.into().trim().to_owned(),
            product_catalog_version: product_catalog_version.into().trim().to_owned(),
            product_catalog_sha256: product_catalog_sha256.into().trim().to_owned(),
            recognition_catalog_version: recognition_catalog_version.into().trim().to_owned(),
            recognition_catalog_sha256: recognition_catalog_sha256.into().trim().to_owned(),
            recognition_template_version: recognition_template_version.into().trim().to_owned(),
        };
        let text_fields = [
            identity.ocr_provider_id.as_str(),
            identity.ocr_provider_version.as_str(),
            identity.product_catalog_version.as_str(),
            identity.recognition_catalog_version.as_str(),
            identity.recognition_template_version.as_str(),
        ];
        let hash_fields = [
            identity.ocr_model_sha256.as_str(),
            identity.ocr_provider_manifest_sha256.as_str(),
            identity.parser_assets_sha256.as_str(),
            identity.product_catalog_sha256.as_str(),
            identity.recognition_catalog_sha256.as_str(),
        ];
        if text_fields
            .iter()
            .any(|value| !is_valid_identity_text(value, 160))
            || hash_fields.iter().any(|value| !is_sha256(value))
        {
            return Err(DomainError::IncompleteObservationIdentity);
        }
        Ok(identity)
    }

    fn matches_provenance(&self, provenance: &CaptureProvenance) -> bool {
        self.ocr_provider_id == provenance.provider_id
            && self.ocr_provider_version == provenance.provider_version
            && self.ocr_model_sha256 == provenance.model_sha256
            && self.ocr_provider_manifest_sha256 == provenance.provider_manifest_sha256
            && self.parser_assets_sha256 == provenance.parser_assets_sha256
    }

    fn stable_components(&self) -> [&str; 10] {
        [
            self.ocr_provider_id.as_str(),
            self.ocr_provider_version.as_str(),
            self.ocr_model_sha256.as_str(),
            self.ocr_provider_manifest_sha256.as_str(),
            self.parser_assets_sha256.as_str(),
            self.product_catalog_version.as_str(),
            self.product_catalog_sha256.as_str(),
            self.recognition_catalog_version.as_str(),
            self.recognition_catalog_sha256.as_str(),
            self.recognition_template_version.as_str(),
        ]
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketContext {
    pub game: Game,
    pub realm: Realm,
    pub league: String,
    pub client_language: ClientLanguage,
    pub game_ui_build: String,
    pub layout_profile_id: String,
    pub layout_profile_version: u32,
    pub parser_version: String,
    pub observation_identity: ObservationIdentity,
}

impl MarketContext {
    pub fn try_new(
        league: impl Into<String>,
        game_ui_build: impl Into<String>,
        layout_profile_id: impl Into<String>,
        layout_profile_version: u32,
        parser_version: impl Into<String>,
        observation_identity: ObservationIdentity,
    ) -> Result<Self, DomainError> {
        let context = Self {
            game: Game::Poe1,
            realm: Realm::Pc,
            league: league.into().trim().to_owned(),
            client_language: ClientLanguage::English,
            game_ui_build: game_ui_build.into().trim().to_owned(),
            layout_profile_id: layout_profile_id.into().trim().to_owned(),
            layout_profile_version,
            parser_version: parser_version.into().trim().to_owned(),
            observation_identity,
        };
        if context.league.is_empty()
            || context.league.len() > 96
            || context.league.contains('|')
            || context.league.chars().any(char::is_control)
            || context.game_ui_build.is_empty()
            || context.game_ui_build.len() > 128
            || context.game_ui_build.contains('|')
            || context.game_ui_build.chars().any(char::is_control)
            || is_placeholder_game_ui_build(&context.game_ui_build)
            || context.layout_profile_id.is_empty()
            || context.layout_profile_id.len() > 160
            || context.layout_profile_id.contains('|')
            || context.layout_profile_id.chars().any(char::is_control)
            || context.layout_profile_version == 0
            || context.parser_version.is_empty()
            || context.parser_version.len() > 160
            || context.parser_version.contains('|')
            || context.parser_version.chars().any(char::is_control)
        {
            return Err(DomainError::IncompleteMarketContext);
        }
        Ok(context)
    }

    #[must_use]
    pub fn stable_key(&self) -> String {
        let digest = Sha256::digest(self.stable_key_material().as_bytes());
        format!("poe1-context-v2:{digest:x}")
    }

    fn stable_key_material(&self) -> String {
        let layout_profile_version = self.layout_profile_version.to_string();
        let mut components = vec![
            "market-context-v2",
            game_key(self.game),
            realm_key(self.realm),
            self.league.as_str(),
            client_language_key(self.client_language),
            self.game_ui_build.as_str(),
            self.layout_profile_id.as_str(),
            layout_profile_version.as_str(),
            self.parser_version.as_str(),
        ];
        components.extend(self.observation_identity.stable_components());
        encode_stable_components(&components)
    }
}

const fn game_key(game: Game) -> &'static str {
    match game {
        Game::Poe1 => "poe1",
    }
}

const fn realm_key(realm: Realm) -> &'static str {
    match realm {
        Realm::Pc => "pc",
    }
}

const fn client_language_key(language: ClientLanguage) -> &'static str {
    match language {
        ClientLanguage::English => "english",
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_valid_identity_text(value: &str, maximum_length: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_length
        && !value.contains('|')
        && !value.chars().any(char::is_control)
}

fn encode_stable_components(components: &[&str]) -> String {
    let mut encoded = String::new();
    for component in components {
        encoded.push_str(&component.len().to_string());
        encoded.push(':');
        encoded.push_str(component);
    }
    encoded
}

fn is_placeholder_game_ui_build(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(normalized.as_str(), "n/a" | "na" | "tbd")
        || ["unknown", "unverified", "placeholder", "unset", "todo"]
            .iter()
            .any(|marker| normalized.contains(marker))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ratio {
    pub text: String,
    pub numerator: u64,
    pub denominator: u64,
}

impl Ratio {
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        let value = value.trim();
        let mut parts = value.split(':');
        let left = parts.next().ok_or(DomainError::InvalidRatio)?;
        let right = parts.next().ok_or(DomainError::InvalidRatio)?;
        if parts.next().is_some() {
            return Err(DomainError::InvalidRatio);
        }
        let left = ExactDecimal::parse(left)?;
        let right = ExactDecimal::parse(right)?;
        let numerator = left
            .coefficient
            .checked_mul(power_of_ten(right.scale)?)
            .ok_or(DomainError::RatioOverflow)?;
        let denominator = right
            .coefficient
            .checked_mul(power_of_ten(left.scale)?)
            .ok_or(DomainError::RatioOverflow)?;
        if numerator == 0 || denominator == 0 {
            return Err(DomainError::InvalidRatio);
        }
        let divisor = greatest_common_divisor(numerator, denominator);
        let numerator = numerator / divisor;
        let denominator = denominator / divisor;
        let numerator = u64::try_from(numerator).map_err(|_| DomainError::RatioOverflow)?;
        let denominator = u64::try_from(denominator).map_err(|_| DomainError::RatioOverflow)?;
        Ok(Self {
            text: format!("{}:{}", left.canonical, right.canonical),
            numerator,
            denominator,
        })
    }

    #[must_use]
    pub fn inverse(&self) -> Self {
        let mut parts = self.text.split(':');
        let left = parts.next().unwrap_or_default();
        let right = parts.next().unwrap_or_default();
        Self {
            text: format!("{right}:{left}"),
            numerator: self.denominator,
            denominator: self.numerator,
        }
    }

    pub fn from_parts(numerator: u64, denominator: u64) -> Result<Self, DomainError> {
        if numerator == 0 || denominator == 0 {
            return Err(DomainError::InvalidRatio);
        }
        let divisor = greatest_common_divisor(u128::from(numerator), u128::from(denominator));
        let numerator = u128::from(numerator) / divisor;
        let denominator = u128::from(denominator) / divisor;
        let numerator = u64::try_from(numerator).map_err(|_| DomainError::RatioOverflow)?;
        let denominator = u64::try_from(denominator).map_err(|_| DomainError::RatioOverflow)?;
        Ok(Self {
            text: format!("{numerator}:{denominator}"),
            numerator,
            denominator,
        })
    }

    #[must_use]
    pub fn compare_value(&self, other: &Self) -> Ordering {
        let left = u128::from(self.numerator) * u128::from(other.denominator);
        let right = u128::from(other.numerator) * u128::from(self.denominator);
        left.cmp(&right)
    }

    #[must_use]
    pub fn differs_by_more_than(&self, other: &Self, factor: u64) -> bool {
        if factor == 0 {
            return true;
        }
        let left = u128::from(self.numerator) * u128::from(other.denominator);
        let right = u128::from(other.numerator) * u128::from(self.denominator);
        left > right * u128::from(factor) || right > left * u128::from(factor)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Amount {
    pub text: String,
    pub numerator: u64,
    pub denominator: u64,
}

impl Amount {
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        let decimal = ExactDecimal::parse(value).map_err(|error| match error {
            DomainError::RatioOverflow => DomainError::AmountOverflow,
            _ => DomainError::InvalidAmount,
        })?;
        let denominator = power_of_ten(decimal.scale).map_err(|_| DomainError::AmountOverflow)?;
        let divisor = greatest_common_divisor(decimal.coefficient, denominator);
        let numerator = u64::try_from(decimal.coefficient / divisor)
            .map_err(|_| DomainError::AmountOverflow)?;
        let denominator =
            u64::try_from(denominator / divisor).map_err(|_| DomainError::AmountOverflow)?;
        Ok(Self {
            text: decimal.canonical,
            numerator,
            denominator,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExactDecimal {
    coefficient: u128,
    scale: u32,
    canonical: String,
}

impl ExactDecimal {
    fn parse(value: &str) -> Result<Self, DomainError> {
        let value = value.trim();
        if value.is_empty() || value.len() > 40 || value.starts_with('+') || value.starts_with('-')
        {
            return Err(DomainError::InvalidRatio);
        }
        let mut split = value.split('.');
        let integer = split.next().ok_or(DomainError::InvalidRatio)?;
        let fraction = split.next();
        if split.next().is_some() {
            return Err(DomainError::InvalidRatio);
        }
        let integer = parse_grouped_digits(integer)?;
        let fraction = match fraction {
            Some(fraction)
                if !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                fraction
            }
            Some(_) => return Err(DomainError::InvalidRatio),
            None => "",
        };
        let joined = format!("{integer}{fraction}");
        let coefficient = joined
            .parse::<u128>()
            .map_err(|_| DomainError::RatioOverflow)?;
        if coefficient == 0 {
            return Err(DomainError::InvalidRatio);
        }
        let canonical_fraction = fraction.trim_end_matches('0').to_owned();
        let canonical_integer = integer.trim_start_matches('0');
        let canonical_integer = if canonical_integer.is_empty() {
            "0"
        } else {
            canonical_integer
        };
        let canonical = if canonical_fraction.is_empty() {
            canonical_integer.to_owned()
        } else {
            format!("{canonical_integer}.{canonical_fraction}")
        };
        let removed = fraction.len().saturating_sub(canonical_fraction.len());
        let divisor =
            power_of_ten(u32::try_from(removed).map_err(|_| DomainError::RatioOverflow)?)?;
        let coefficient = coefficient / divisor;
        Ok(Self {
            coefficient,
            scale: u32::try_from(fraction.len().saturating_sub(removed))
                .map_err(|_| DomainError::RatioOverflow)?,
            canonical,
        })
    }
}

fn parse_grouped_digits(value: &str) -> Result<String, DomainError> {
    if value.is_empty() {
        return Err(DomainError::InvalidRatio);
    }
    if !value.contains(',') {
        return value
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| value.to_owned())
            .ok_or(DomainError::InvalidRatio);
    }
    let groups = value.split(',').collect::<Vec<_>>();
    if groups.is_empty()
        || groups[0].is_empty()
        || groups[0].len() > 3
        || !groups[0].bytes().all(|byte| byte.is_ascii_digit())
        || groups[1..]
            .iter()
            .any(|group| group.len() != 3 || !group.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(DomainError::InvalidRatio);
    }
    Ok(groups.concat())
}

fn power_of_ten(scale: u32) -> Result<u128, DomainError> {
    10_u128.checked_pow(scale).ok_or(DomainError::RatioOverflow)
}

const fn greatest_common_divisor(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteSide {
    Available,
    Competing,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    Exact,
    LessThan,
    GreaterThan,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionType {
    Taker,
    MakerReference,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteEdgeRole {
    AvailableTaker,
    AvailableReverseMakerReference,
    CompetingMakerReference,
    CompetingReverseTaker,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedOrderRow {
    pub side: QuoteSide,
    pub row_index: u8,
    pub comparator: Comparator,
    pub ratio: Ratio,
    pub stock: u64,
    pub user_edited: bool,
    pub machine_confidence_ppm: Option<u32>,
}

impl ConfirmedOrderRow {
    pub fn try_new(
        side: QuoteSide,
        row_index: u8,
        comparator: Comparator,
        ratio_text: &str,
        stock_text: &str,
        user_edited: bool,
        machine_confidence_ppm: Option<u32>,
    ) -> Result<Self, DomainError> {
        let stock = parse_positive_stock(stock_text)?;
        if machine_confidence_ppm.is_some_and(|value| value > 1_000_000) {
            return Err(DomainError::InvalidMachineConfidence);
        }
        Ok(Self {
            side,
            row_index,
            comparator,
            ratio: Ratio::parse(ratio_text)?,
            stock,
            user_edited,
            machine_confidence_ppm,
        })
    }
}

fn parse_positive_stock(value: &str) -> Result<u64, DomainError> {
    let digits = parse_grouped_digits(value.trim()).map_err(|_| DomainError::InvalidStock)?;
    let stock = digits
        .parse::<u64>()
        .map_err(|_| DomainError::InvalidStock)?;
    if stock == 0 {
        return Err(DomainError::InvalidStock);
    }
    Ok(stock)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureConfirmationMode {
    AutomaticConsensus,
    UserReviewed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureProvenance {
    pub draft_id: String,
    pub capture_job_id: String,
    pub review_revision: u32,
    pub confirmation_mode: CaptureConfirmationMode,
    pub source: String,
    pub evidence_id: String,
    pub evidence_removed: bool,
    pub frame_hashes: Vec<String>,
    pub profile_sha256: String,
    pub provider_id: String,
    pub provider_version: String,
    pub provider_manifest_sha256: String,
    pub model_sha256: String,
    pub parser_assets_sha256: String,
}

impl CaptureProvenance {
    fn validate(&self) -> Result<(), DomainError> {
        let required = [
            self.draft_id.as_str(),
            self.capture_job_id.as_str(),
            self.source.as_str(),
            self.evidence_id.as_str(),
            self.profile_sha256.as_str(),
            self.provider_id.as_str(),
            self.provider_version.as_str(),
            self.provider_manifest_sha256.as_str(),
            self.model_sha256.as_str(),
            self.parser_assets_sha256.as_str(),
        ];
        if self.review_revision == 0
            || self.frame_hashes.len() != 2
            || required.iter().any(|value| value.trim().is_empty())
            || self
                .frame_hashes
                .iter()
                .any(|hash| hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(DomainError::IncompleteProvenance);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewAuditEntry {
    pub field_path: String,
    pub before: Value,
    pub after: Value,
    pub kind: ReviewAuditKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAuditKind {
    AcceptedMachineValue,
    UserCorrection,
    UserExclusion,
    UserInclusion,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmedCapture {
    pub capture_id: String,
    pub snapshot_id: String,
    pub confirmed_at: DateTime<Utc>,
    pub captured_at: DateTime<Utc>,
    pub context: MarketContext,
    pub need_asset_id: MarketAssetId,
    pub have_asset_id: MarketAssetId,
    pub rows: Vec<ConfirmedOrderRow>,
    pub provenance: CaptureProvenance,
    pub machine_draft_json: String,
    pub review_json: String,
    pub audit_entries: Vec<ReviewAuditEntry>,
    pub quotes: Vec<MarketQuote>,
    pub quote_edges: Vec<QuoteEdge>,
}

impl ConfirmedCapture {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        captured_at: DateTime<Utc>,
        context: MarketContext,
        need_asset_id: MarketAssetId,
        have_asset_id: MarketAssetId,
        rows: Vec<ConfirmedOrderRow>,
        provenance: CaptureProvenance,
        machine_draft_json: String,
        review_json: String,
        audit_entries: Vec<ReviewAuditEntry>,
    ) -> Result<Self, DomainError> {
        provenance.validate()?;
        if !context.observation_identity.matches_provenance(&provenance) {
            return Err(DomainError::ObservationIdentityMismatch);
        }
        if need_asset_id == have_asset_id {
            return Err(DomainError::SameMarketAsset);
        }
        if rows.is_empty() {
            return Err(DomainError::NoConfirmedRows);
        }
        if machine_draft_json.trim().is_empty() || review_json.trim().is_empty() {
            return Err(DomainError::IncompleteProvenance);
        }
        let mut row_keys = std::collections::BTreeSet::new();
        if rows
            .iter()
            .any(|row| !row_keys.insert((row.side as u8, row.row_index)))
        {
            return Err(DomainError::DuplicateConfirmedRow);
        }
        if audit_entries
            .iter()
            .any(|entry| entry.field_path.trim().is_empty())
        {
            return Err(DomainError::InvalidAuditEntry);
        }

        let capture_id = Uuid::new_v4().to_string();
        let snapshot_id = Uuid::new_v4().to_string();
        let confirmed_at = Utc::now();
        let mut quotes = Vec::with_capacity(rows.len());
        let mut quote_edges = Vec::with_capacity(rows.len() * 2);
        for row in &rows {
            let quote_id = Uuid::new_v4().to_string();
            quotes.push(MarketQuote {
                quote_id: quote_id.clone(),
                snapshot_id: snapshot_id.clone(),
                context_key: context.stable_key(),
                side: row.side,
                row_index: row.row_index,
                comparator: row.comparator,
                ratio: row.ratio.clone(),
                stock: row.stock,
                user_edited: row.user_edited,
                machine_confidence_ppm: row.machine_confidence_ppm,
                confirmed_at,
            });
            quote_edges.extend(build_quote_edges(
                &snapshot_id,
                &quote_id,
                &context,
                &need_asset_id,
                &have_asset_id,
                row,
                captured_at,
                confirmed_at,
            ));
        }
        Ok(Self {
            capture_id,
            snapshot_id,
            confirmed_at,
            captured_at,
            context,
            need_asset_id,
            have_asset_id,
            rows,
            provenance,
            machine_draft_json,
            review_json,
            audit_entries,
            quotes,
            quote_edges,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketQuote {
    pub quote_id: String,
    pub snapshot_id: String,
    pub context_key: String,
    pub side: QuoteSide,
    pub row_index: u8,
    pub comparator: Comparator,
    pub ratio: Ratio,
    pub stock: u64,
    pub user_edited: bool,
    pub machine_confidence_ppm: Option<u32>,
    pub confirmed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteEdge {
    pub edge_id: String,
    pub snapshot_id: String,
    pub quote_id: String,
    pub context_key: String,
    pub from_asset_id: MarketAssetId,
    pub to_asset_id: MarketAssetId,
    pub rate: Ratio,
    pub source_side: QuoteSide,
    pub execution_type: ExecutionType,
    pub role: QuoteEdgeRole,
    pub stock: u64,
    pub original_need_asset_id: MarketAssetId,
    pub original_have_asset_id: MarketAssetId,
    pub original_row_index: u8,
    pub comparator: Comparator,
    pub user_edited: bool,
    pub machine_confidence_ppm: Option<u32>,
    pub captured_at: DateTime<Utc>,
    pub confirmed_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
fn build_quote_edges(
    snapshot_id: &str,
    quote_id: &str,
    context: &MarketContext,
    need_asset_id: &MarketAssetId,
    have_asset_id: &MarketAssetId,
    row: &ConfirmedOrderRow,
    captured_at: DateTime<Utc>,
    confirmed_at: DateTime<Utc>,
) -> [QuoteEdge; 2] {
    let (current_execution, current_role, reverse_execution, reverse_role) = match row.side {
        QuoteSide::Available => (
            ExecutionType::Taker,
            QuoteEdgeRole::AvailableTaker,
            ExecutionType::MakerReference,
            QuoteEdgeRole::AvailableReverseMakerReference,
        ),
        QuoteSide::Competing => (
            ExecutionType::MakerReference,
            QuoteEdgeRole::CompetingMakerReference,
            ExecutionType::Taker,
            QuoteEdgeRole::CompetingReverseTaker,
        ),
    };
    [
        QuoteEdge {
            edge_id: Uuid::new_v4().to_string(),
            snapshot_id: snapshot_id.to_owned(),
            quote_id: quote_id.to_owned(),
            context_key: context.stable_key(),
            from_asset_id: have_asset_id.clone(),
            to_asset_id: need_asset_id.clone(),
            rate: row.ratio.clone(),
            source_side: row.side,
            execution_type: current_execution,
            role: current_role,
            stock: row.stock,
            original_need_asset_id: need_asset_id.clone(),
            original_have_asset_id: have_asset_id.clone(),
            original_row_index: row.row_index,
            comparator: row.comparator,
            user_edited: row.user_edited,
            machine_confidence_ppm: row.machine_confidence_ppm,
            captured_at,
            confirmed_at,
        },
        QuoteEdge {
            edge_id: Uuid::new_v4().to_string(),
            snapshot_id: snapshot_id.to_owned(),
            quote_id: quote_id.to_owned(),
            context_key: context.stable_key(),
            from_asset_id: need_asset_id.clone(),
            to_asset_id: have_asset_id.clone(),
            rate: row.ratio.inverse(),
            source_side: row.side,
            execution_type: reverse_execution,
            role: reverse_role,
            stock: row.stock,
            original_need_asset_id: need_asset_id.clone(),
            original_have_asset_id: have_asset_id.clone(),
            original_row_index: row.row_index,
            comparator: row.comparator,
            user_edited: row.user_edited,
            machine_confidence_ppm: row.machine_confidence_ppm,
            captured_at,
            confirmed_at,
        },
    ]
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRecordStatus {
    Active,
    Isolated,
    Deleted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketEdgeObservation {
    pub edge: QuoteEdge,
    pub snapshot_complete: bool,
    pub record_status: SnapshotRecordStatus,
    pub record_revision: u32,
    pub record_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketAssetCategoryDefinition {
    pub category_id: String,
    pub name_en: String,
    pub name_zh_cn: Option<String>,
    pub aliases_en: Vec<String>,
    pub aliases_zh_cn: Vec<String>,
    pub sort_order: u32,
    pub catalog_version: String,
    pub catalog_status: String,
}

impl MarketAssetCategoryDefinition {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        category_id: impl Into<String>,
        name_en: impl Into<String>,
        name_zh_cn: Option<String>,
        aliases_en: Vec<String>,
        aliases_zh_cn: Vec<String>,
        sort_order: u32,
        catalog_version: impl Into<String>,
        catalog_status: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let category_id = category_id.into();
        MarketAssetId::try_new(category_id.clone())
            .map_err(|_| DomainError::InvalidMarketAssetCategoryDefinition)?;
        let definition = Self {
            category_id,
            name_en: name_en.into().trim().to_owned(),
            name_zh_cn: trimmed_optional(name_zh_cn),
            aliases_en: trimmed_values(aliases_en)
                .map_err(|_| DomainError::InvalidMarketAssetCategoryDefinition)?,
            aliases_zh_cn: trimmed_values(aliases_zh_cn)
                .map_err(|_| DomainError::InvalidMarketAssetCategoryDefinition)?,
            sort_order,
            catalog_version: catalog_version.into().trim().to_owned(),
            catalog_status: catalog_status.into().trim().to_owned(),
        };
        if definition.name_en.is_empty()
            || definition.catalog_version.is_empty()
            || !valid_catalog_status(&definition.catalog_status)
        {
            return Err(DomainError::InvalidMarketAssetCategoryDefinition);
        }
        Ok(definition)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketAssetDefinition {
    pub market_asset_id: MarketAssetId,
    pub english_name: String,
    pub display_name_zh_cn: Option<String>,
    pub category_id: Option<String>,
    pub aliases_en: Vec<String>,
    pub aliases_zh_cn: Vec<String>,
    pub icon_sha256: String,
    pub icon_relative_path: Option<String>,
    pub icon_width: Option<u32>,
    pub icon_height: Option<u32>,
    pub minimum_unit_numerator: u64,
    pub minimum_unit_denominator: u64,
    pub sort_order: u32,
    pub catalog_version: String,
    pub catalog_status: String,
    pub enabled: bool,
}

impl MarketAssetDefinition {
    pub fn try_new(
        market_asset_id: impl Into<String>,
        english_name: impl Into<String>,
        icon_sha256: impl Into<String>,
        catalog_version: impl Into<String>,
        catalog_status: impl Into<String>,
    ) -> Result<Self, DomainError> {
        Self::try_new_catalog(
            market_asset_id,
            english_name,
            None,
            None,
            Vec::new(),
            Vec::new(),
            icon_sha256,
            None,
            None,
            None,
            1,
            1,
            0,
            catalog_version,
            catalog_status,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn try_new_catalog(
        market_asset_id: impl Into<String>,
        english_name: impl Into<String>,
        display_name_zh_cn: Option<String>,
        category_id: Option<String>,
        aliases_en: Vec<String>,
        aliases_zh_cn: Vec<String>,
        icon_sha256: impl Into<String>,
        icon_relative_path: Option<String>,
        icon_width: Option<u32>,
        icon_height: Option<u32>,
        minimum_unit_numerator: u64,
        minimum_unit_denominator: u64,
        sort_order: u32,
        catalog_version: impl Into<String>,
        catalog_status: impl Into<String>,
        enabled: bool,
    ) -> Result<Self, DomainError> {
        let definition = Self {
            market_asset_id: MarketAssetId::try_new(market_asset_id)?,
            english_name: english_name.into().trim().to_owned(),
            display_name_zh_cn: trimmed_optional(display_name_zh_cn),
            category_id: trimmed_optional(category_id),
            aliases_en: trimmed_values(aliases_en)?,
            aliases_zh_cn: trimmed_values(aliases_zh_cn)?,
            icon_sha256: icon_sha256.into().trim().to_ascii_lowercase(),
            icon_relative_path: trimmed_optional(icon_relative_path),
            icon_width,
            icon_height,
            minimum_unit_numerator,
            minimum_unit_denominator,
            sort_order,
            catalog_version: catalog_version.into().trim().to_owned(),
            catalog_status: catalog_status.into().trim().to_owned(),
            enabled,
        };
        if let Some(category_id) = &definition.category_id {
            MarketAssetId::try_new(category_id.clone())
                .map_err(|_| DomainError::InvalidMarketAssetDefinition)?;
        }
        let dimensions_are_consistent = matches!(
            (definition.icon_width, definition.icon_height),
            (None, None) | (Some(1..), Some(1..))
        );
        if definition.english_name.is_empty()
            || definition.icon_sha256.len() != 64
            || !definition
                .icon_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !dimensions_are_consistent
            || definition.minimum_unit_numerator == 0
            || definition.minimum_unit_denominator == 0
            || greatest_common_divisor(
                u128::from(definition.minimum_unit_numerator),
                u128::from(definition.minimum_unit_denominator),
            ) != 1
            || definition.catalog_version.is_empty()
            || !valid_catalog_status(&definition.catalog_status)
        {
            return Err(DomainError::InvalidMarketAssetDefinition);
        }
        Ok(definition)
    }
}

fn valid_catalog_status(value: &str) -> bool {
    matches!(value, "manual_only" | "formal")
}

fn trimmed_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn trimmed_values(values: Vec<String>) -> Result<Vec<String>, DomainError> {
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let value = value.trim().to_owned();
        if value.is_empty()
            || normalized
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&value))
        {
            return Err(DomainError::InvalidMarketAssetDefinition);
        }
        normalized.push(value);
    }
    Ok(normalized)
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DomainError {
    #[error("market asset identity is invalid")]
    InvalidMarketAssetId,
    #[error("market asset definition is invalid")]
    InvalidMarketAssetDefinition,
    #[error("market asset category definition is invalid")]
    InvalidMarketAssetCategoryDefinition,
    #[error("market context is incomplete")]
    IncompleteMarketContext,
    #[error("observation identity is incomplete")]
    IncompleteObservationIdentity,
    #[error("market context observation identity does not match capture provenance")]
    ObservationIdentityMismatch,
    #[error("ratio must be a strict positive X:Y value")]
    InvalidRatio,
    #[error("ratio exceeds the exact numeric range")]
    RatioOverflow,
    #[error("amount must be a strict positive decimal value")]
    InvalidAmount,
    #[error("amount exceeds the exact numeric range")]
    AmountOverflow,
    #[error("stock must be a strict positive integer")]
    InvalidStock,
    #[error("machine confidence is outside the valid range")]
    InvalidMachineConfidence,
    #[error("capture provenance is incomplete")]
    IncompleteProvenance,
    #[error("Need and Have must be different assets")]
    SameMarketAsset,
    #[error("a confirmed capture requires at least one row")]
    NoConfirmedRows,
    #[error("confirmed capture rows must be unique by side and index")]
    DuplicateConfirmedRow,
    #[error("review audit entry is invalid")]
    InvalidAuditEntry,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;

    fn provenance() -> CaptureProvenance {
        CaptureProvenance {
            draft_id: Uuid::new_v4().to_string(),
            capture_job_id: Uuid::new_v4().to_string(),
            review_revision: 2,
            confirmation_mode: CaptureConfirmationMode::UserReviewed,
            source: "live_wgc_exact_hwnd".to_owned(),
            evidence_id: "evidence-1".to_owned(),
            evidence_removed: false,
            frame_hashes: vec!["a".repeat(64), "b".repeat(64)],
            profile_sha256: "c".repeat(64),
            provider_id: "provider".to_owned(),
            provider_version: "v1".to_owned(),
            provider_manifest_sha256: "d".repeat(64),
            model_sha256: "e".repeat(64),
            parser_assets_sha256: "f".repeat(64),
        }
    }

    fn context() -> MarketContext {
        MarketContext::try_new(
            "Standard",
            "3.27.0",
            "layout",
            1,
            "parser",
            ObservationIdentity::try_new(
                "provider",
                "v1",
                "e".repeat(64),
                "d".repeat(64),
                "f".repeat(64),
                "product-v1",
                "1".repeat(64),
                "recognition-v1",
                "2".repeat(64),
                "template-v1",
            )
            .expect("observation identity"),
        )
        .expect("context")
    }

    #[test]
    fn ratio_parser_is_exact_and_reduces_decimals_without_float() {
        assert_eq!(
            Ratio::parse(" 01.50 : 2.0 ").expect("ratio"),
            Ratio {
                text: "1.5:2".to_owned(),
                numerator: 3,
                denominator: 4,
            }
        );
        assert_eq!(
            Ratio::parse("1,000.50:2").expect("grouped ratio"),
            Ratio {
                text: "1000.5:2".to_owned(),
                numerator: 2001,
                denominator: 4,
            }
        );
        assert_eq!(Ratio::parse("1O:2"), Err(DomainError::InvalidRatio));
        assert_eq!(Ratio::parse("1/2"), Err(DomainError::InvalidRatio));
        assert_eq!(
            Ratio::from_parts(10, 20).expect("parts"),
            Ratio {
                text: "1:2".to_owned(),
                numerator: 1,
                denominator: 2,
            }
        );
        assert_eq!(
            Ratio::parse("9007199254740991:9007199254740990")
                .expect("large exact ratio")
                .compare_value(
                    &Ratio::parse("9007199254740990:9007199254740991")
                        .expect("large inverse ratio")
                ),
            Ordering::Greater
        );
    }

    #[test]
    fn amount_parser_preserves_decimal_values_as_reduced_rationals() {
        assert_eq!(
            Amount::parse("1,250.50").expect("amount"),
            Amount {
                text: "1250.5".to_owned(),
                numerator: 2501,
                denominator: 2,
            }
        );
        assert_eq!(Amount::parse("0"), Err(DomainError::InvalidAmount));
    }

    #[test]
    fn confirmed_rows_generate_have_to_need_and_role_safe_reverse_edges() {
        let row = ConfirmedOrderRow::try_new(
            QuoteSide::Available,
            0,
            Comparator::Exact,
            "3:2",
            "1,000",
            false,
            Some(990_000),
        )
        .expect("row");
        let capture = ConfirmedCapture::try_new(
            Utc::now(),
            context(),
            MarketAssetId::try_new("divine-orb").expect("need"),
            MarketAssetId::try_new("chaos-orb").expect("have"),
            vec![row],
            provenance(),
            "{}".to_owned(),
            "{}".to_owned(),
            vec![ReviewAuditEntry {
                field_path: "need".to_owned(),
                before: json!("divine-orb"),
                after: json!("divine-orb"),
                kind: ReviewAuditKind::AcceptedMachineValue,
            }],
        )
        .expect("confirmed capture");
        assert_eq!(capture.quotes.len(), 1);
        assert_eq!(capture.quote_edges.len(), 2);
        let current = &capture.quote_edges[0];
        assert_eq!(current.from_asset_id.as_str(), "chaos-orb");
        assert_eq!(current.to_asset_id.as_str(), "divine-orb");
        assert_eq!(current.rate.numerator, 3);
        assert_eq!(current.rate.denominator, 2);
        assert_eq!(current.execution_type, ExecutionType::Taker);
        assert_eq!(current.role, QuoteEdgeRole::AvailableTaker);
        let reverse = &capture.quote_edges[1];
        assert_eq!(reverse.from_asset_id.as_str(), "divine-orb");
        assert_eq!(reverse.to_asset_id.as_str(), "chaos-orb");
        assert_eq!(reverse.rate.numerator, 2);
        assert_eq!(reverse.rate.denominator, 3);
        assert_eq!(reverse.execution_type, ExecutionType::MakerReference);
    }

    #[test]
    fn context_and_confirmation_fail_closed() {
        let identity = context().observation_identity;
        assert_eq!(
            MarketContext::try_new("", "build", "layout", 1, "parser", identity.clone()),
            Err(DomainError::IncompleteMarketContext)
        );
        assert_eq!(
            MarketContext::try_new(
                "Standard\nHardcore",
                "3.27.0",
                "layout",
                1,
                "parser",
                identity.clone(),
            ),
            Err(DomainError::IncompleteMarketContext)
        );
        for placeholder in [
            "unknown-manual-unverified",
            "UNKNOWN",
            "placeholder-build",
            "n/a",
            "tbd",
        ] {
            assert_eq!(
                MarketContext::try_new(
                    "Standard",
                    placeholder,
                    "layout",
                    1,
                    "parser",
                    identity.clone(),
                ),
                Err(DomainError::IncompleteMarketContext),
                "placeholder build must not form a confirmable context: {placeholder}",
            );
        }
        let asset = MarketAssetId::try_new("chaos-orb").expect("asset");
        assert_eq!(
            ConfirmedCapture::try_new(
                Utc::now(),
                context(),
                asset.clone(),
                asset,
                Vec::new(),
                provenance(),
                "{}".to_owned(),
                "{}".to_owned(),
                Vec::new(),
            ),
            Err(DomainError::SameMarketAsset)
        );
    }

    #[test]
    fn observation_identity_changes_the_market_context_partition() {
        let first = context();
        let mut different_model = first.clone();
        different_model.observation_identity.ocr_model_sha256 = "9".repeat(64);
        let mut different_product_catalog = first.clone();
        different_product_catalog
            .observation_identity
            .product_catalog_version = "product-v2".to_owned();
        let mut different_recognition_catalog = first.clone();
        different_recognition_catalog
            .observation_identity
            .recognition_catalog_sha256 = "8".repeat(64);

        assert_ne!(first.stable_key(), different_model.stable_key());
        assert_ne!(first.stable_key(), different_product_catalog.stable_key());
        assert_ne!(
            first.stable_key(),
            different_recognition_catalog.stable_key()
        );
        assert!(first.stable_key().starts_with("poe1-context-v2:"));
        let material = first.stable_key_material();
        assert!(material.starts_with("17:market-context-v24:poe12:pc8:Standard7:english"));
        assert!(material.contains("10:product-v1"));
        assert!(material.contains("14:recognition-v1"));
        assert_ne!(
            encode_stable_components(&["ab", "c"]),
            encode_stable_components(&["a", "bc"]),
            "length prefixes must distinguish adjacent-component collisions"
        );
    }

    #[test]
    fn observation_identity_rejects_control_characters() {
        assert_eq!(
            ObservationIdentity::try_new(
                "provider\nforged",
                "v1",
                "e".repeat(64),
                "d".repeat(64),
                "f".repeat(64),
                "product-v1",
                "1".repeat(64),
                "recognition-v1",
                "2".repeat(64),
                "template-v1",
            ),
            Err(DomainError::IncompleteObservationIdentity)
        );
    }

    #[test]
    fn confirmed_capture_rejects_context_provenance_identity_drift() {
        let mut mismatched = context();
        mismatched.observation_identity.ocr_model_sha256 = "9".repeat(64);
        let asset = MarketAssetId::try_new("chaos-orb").expect("asset");
        assert_eq!(
            ConfirmedCapture::try_new(
                Utc::now(),
                mismatched,
                MarketAssetId::try_new("divine-orb").expect("need"),
                asset,
                vec![
                    ConfirmedOrderRow::try_new(
                        QuoteSide::Available,
                        0,
                        Comparator::Exact,
                        "1:1",
                        "1",
                        false,
                        Some(1_000_000),
                    )
                    .expect("row")
                ],
                provenance(),
                "{}".to_owned(),
                "{}".to_owned(),
                Vec::new(),
            ),
            Err(DomainError::ObservationIdentityMismatch)
        );
    }

    #[test]
    fn market_asset_id_deserialization_reuses_domain_validation() {
        let valid: MarketAssetId =
            serde_json::from_str("\"chaos-orb\"").expect("valid market asset id");
        assert_eq!(valid.as_str(), "chaos-orb");

        for invalid in [
            "\"Chaos Orb\"",
            "\"-chaos-orb\"",
            "\"chaos--orb\"",
            "\"chaos_orb\"",
            "\"\"",
        ] {
            assert!(
                serde_json::from_str::<MarketAssetId>(invalid).is_err(),
                "invalid identifier must fail deserialization: {invalid}"
            );
        }
    }
}
