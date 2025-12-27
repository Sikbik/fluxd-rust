//! Monetary units and money range rules.

pub type Amount = i64;

pub const COIN: Amount = 100_000_000;
pub const CENT: Amount = 1_000_000;

/// No amount larger than this (in satoshi) is valid.
pub const MAX_MONEY: Amount = 440_000_000 * COIN;

pub fn money_range(value: Amount) -> bool {
    (0..=MAX_MONEY).contains(&value)
}
