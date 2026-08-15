use std::collections::BTreeMap;

use ptt_trade_domain::MarketAssetId;
use serde::{Deserialize, Serialize};

use crate::EngineError;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetUnit {
    pub numerator: u64,
    pub denominator: u64,
}

impl AssetUnit {
    pub fn try_new(numerator: u64, denominator: u64) -> Result<Self, EngineError> {
        if numerator == 0 || denominator == 0 {
            return Err(EngineError::InvalidAssetUnit);
        }
        let divisor = greatest_common_divisor(numerator, denominator);
        Ok(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    #[must_use]
    pub const fn whole() -> Self {
        Self {
            numerator: 1,
            denominator: 1,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetUnitCatalog {
    units: BTreeMap<MarketAssetId, AssetUnit>,
}

impl AssetUnitCatalog {
    pub fn try_new(
        units: impl IntoIterator<Item = (MarketAssetId, AssetUnit)>,
    ) -> Result<Self, EngineError> {
        let mut catalog = BTreeMap::new();
        for (asset_id, unit) in units {
            if catalog.insert(asset_id, unit).is_some() {
                return Err(EngineError::InvalidAssetUnitCatalog);
            }
        }
        if catalog.is_empty() {
            return Err(EngineError::InvalidAssetUnitCatalog);
        }
        Ok(Self { units: catalog })
    }

    pub fn unit(&self, asset_id: &MarketAssetId) -> Result<AssetUnit, EngineError> {
        self.units
            .get(asset_id)
            .copied()
            .ok_or(EngineError::InvalidAssetUnitCatalog)
    }

    #[must_use]
    pub fn contains(&self, asset_id: &MarketAssetId) -> bool {
        self.units.contains_key(asset_id)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetAmount {
    pub asset_id: MarketAssetId,
    pub quanta: u64,
    pub unit: AssetUnit,
}

impl AssetAmount {
    pub fn from_whole_units(
        asset_id: MarketAssetId,
        whole_units: u64,
        catalog: &AssetUnitCatalog,
    ) -> Result<Self, EngineError> {
        if whole_units == 0 {
            return Err(EngineError::NonPositiveAmount);
        }
        let unit = catalog.unit(&asset_id)?;
        let scaled = u128::from(whole_units)
            .checked_mul(u128::from(unit.denominator))
            .ok_or(EngineError::NumericOverflow)?;
        if scaled % u128::from(unit.numerator) != 0 {
            return Err(EngineError::AmountNotAligned);
        }
        let quanta = u64::try_from(scaled / u128::from(unit.numerator))
            .map_err(|_| EngineError::NumericOverflow)?;
        Self::from_quanta(asset_id, quanta, catalog)
    }

    pub fn from_quanta(
        asset_id: MarketAssetId,
        quanta: u64,
        catalog: &AssetUnitCatalog,
    ) -> Result<Self, EngineError> {
        Ok(Self {
            unit: catalog.unit(&asset_id)?,
            asset_id,
            quanta,
        })
    }

    pub(crate) fn zero(
        asset_id: MarketAssetId,
        catalog: &AssetUnitCatalog,
    ) -> Result<Self, EngineError> {
        Self::from_quanta(asset_id, 0, catalog)
    }

    pub(crate) fn checked_add_quanta(&mut self, value: u64) -> Result<(), EngineError> {
        self.quanta = self
            .quanta
            .checked_add(value)
            .ok_or(EngineError::NumericOverflow)?;
        Ok(())
    }

    #[must_use]
    pub fn exact_display_fraction(&self) -> (u128, u128) {
        let numerator = u128::from(self.quanta) * u128::from(self.unit.numerator);
        let denominator = u128::from(self.unit.denominator);
        let divisor = greatest_common_divisor_u128(numerator, denominator);
        (numerator / divisor, denominator / divisor)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeePolicy {
    Unknown,
    None,
    OutputFraction { numerator: u64, denominator: u64 },
}

impl FeePolicy {
    pub fn validate(self) -> Result<(), EngineError> {
        if let Self::OutputFraction {
            numerator,
            denominator,
        } = self
            && (denominator == 0 || numerator >= denominator)
        {
            return Err(EngineError::InvalidFeePolicy);
        }
        Ok(())
    }

    pub(crate) fn apply_to_quanta(self, gross_quanta: u64) -> Result<Option<u64>, EngineError> {
        self.validate()?;
        match self {
            Self::Unknown => Ok(None),
            Self::None => Ok(Some(gross_quanta)),
            Self::OutputFraction {
                numerator,
                denominator,
            } => {
                let retained = denominator - numerator;
                let net = u128::from(gross_quanta)
                    .checked_mul(u128::from(retained))
                    .ok_or(EngineError::NumericOverflow)?
                    / u128::from(denominator);
                Ok(Some(
                    u64::try_from(net).map_err(|_| EngineError::NumericOverflow)?,
                ))
            }
        }
    }

    #[must_use]
    pub const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

pub(crate) const fn greatest_common_divisor(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

const fn greatest_common_divisor_u128(mut left: u128, mut right: u128) -> u128 {
    if left == 0 {
        return if right == 0 { 1 } else { right };
    }
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(value: &str) -> MarketAssetId {
        MarketAssetId::try_new(value).expect("asset")
    }

    #[test]
    fn minimum_units_quantize_without_float() {
        let catalog = AssetUnitCatalog::try_new([
            (asset("whole"), AssetUnit::whole()),
            (asset("half"), AssetUnit::try_new(1, 2).expect("half unit")),
            (
                asset("double"),
                AssetUnit::try_new(2, 1).expect("double unit"),
            ),
        ])
        .expect("catalog");
        assert_eq!(
            AssetAmount::from_whole_units(asset("half"), 3, &catalog)
                .expect("amount")
                .quanta,
            6
        );
        assert_eq!(
            AssetAmount::from_whole_units(asset("double"), 1, &catalog),
            Err(EngineError::AmountNotAligned)
        );
    }

    #[test]
    fn known_fee_floors_in_target_quanta_and_unknown_fee_stays_unknown() {
        assert_eq!(
            FeePolicy::OutputFraction {
                numerator: 1,
                denominator: 10,
            }
            .apply_to_quanta(9)
            .expect("fee"),
            Some(8)
        );
        assert_eq!(
            FeePolicy::Unknown.apply_to_quanta(9).expect("unknown fee"),
            None
        );
    }
}
