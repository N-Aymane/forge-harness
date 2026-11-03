//! Checked integer accounting. Monetary values are billionths of a US dollar.
use crate::model::Usage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    pub input_nusd_per_token: u64,
    pub output_nusd_per_token: u64,
    pub cached_input_nusd_per_token: u64,
    #[serde(default)]
    pub cache_write_nusd_per_token: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum AccountingError {
    #[error("monetary arithmetic overflow")]
    Overflow,
    #[error("cache-write pricing is required for reported cache writes")]
    MissingCachePrice,
    #[error("invalid USD amount (use up to nine decimal places)")]
    InvalidAmount,
}

impl Pricing {
    pub fn cost(&self, usage: &Usage) -> Result<u64, AccountingError> {
        let ordinary = usage.input.saturating_sub(usage.cached_input);
        let writes = match (usage.cache_write, self.cache_write_nusd_per_token) {
            (0, _) => 0,
            (_, Some(rate)) => usage
                .cache_write
                .checked_mul(rate)
                .ok_or(AccountingError::Overflow)?,
            (_, None) => return Err(AccountingError::MissingCachePrice),
        };
        let terms = [
            (ordinary, self.input_nusd_per_token),
            (usage.cached_input, self.cached_input_nusd_per_token),
            (usage.output, self.output_nusd_per_token),
        ];
        terms.into_iter().try_fold(writes, |sum, (count, rate)| {
            sum.checked_add(count.checked_mul(rate).ok_or(AccountingError::Overflow)?)
                .ok_or(AccountingError::Overflow)
        })
    }

    pub fn reserve(&self, input: u64, output: u64) -> Result<u64, AccountingError> {
        let input_rate = self
            .input_nusd_per_token
            .max(self.cached_input_nusd_per_token)
            .max(self.cache_write_nusd_per_token.unwrap_or(0));
        input
            .checked_mul(input_rate)
            .and_then(|x| {
                output
                    .checked_mul(self.output_nusd_per_token)
                    .and_then(|y| x.checked_add(y))
            })
            .ok_or(AccountingError::Overflow)
    }
}

pub fn parse_usd(value: &str) -> Result<u64, AccountingError> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(AccountingError::InvalidAmount);
    }
    let whole: u64 = whole.parse().map_err(|_| AccountingError::InvalidAmount)?;
    let frac: u64 = format!("{fraction:0<9}")
        .parse()
        .map_err(|_| AccountingError::InvalidAmount)?;
    whole
        .checked_mul(1_000_000_000)
        .and_then(|x| x.checked_add(frac))
        .ok_or(AccountingError::Overflow)
}

pub fn usd(amount: Option<u64>) -> String {
    amount
        .map(|n| format!("${}.{:09}", n / 1_000_000_000, n % 1_000_000_000))
        .unwrap_or_else(|| "unpriced".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_round_trip_and_overflow() {
        assert_eq!(parse_usd("1.000000001").unwrap(), 1_000_000_001);
        assert!(parse_usd("0.0000000001").is_err());
        assert!(parse_usd("18446744073709551615").is_err());
        assert_eq!(usd(None), "unpriced");
    }
    #[test]
    fn cached_and_reasoning_not_double_counted() {
        let p = Pricing {
            input_nusd_per_token: 10,
            output_nusd_per_token: 20,
            cached_input_nusd_per_token: 2,
            cache_write_nusd_per_token: Some(15),
        };
        let u = Usage {
            input: 100,
            output: 20,
            cached_input: 30,
            cache_write: 10,
            reasoning: 5,
            ..Default::default()
        };
        assert_eq!(p.cost(&u).unwrap(), 1310);
        assert!(p.reserve(u64::MAX, 10).is_err());
    }
}
