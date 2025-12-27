//! Consensus-wide constants shared across validation.

/// The minimum allowed block version (network rule).
pub const MIN_BLOCK_VERSION: i32 = 4;
/// The minimum allowed block version once PON activates.
pub const MIN_PON_BLOCK_VERSION: i32 = 100;
/// Maximum reorg length accepted under normal conditions.
pub const MAX_REORG_LENGTH: i64 = 40;
/// Temporary deeper reorg limit during PON stabilization.
pub const PON_REORG_LENGTH: i64 = 5_000;
pub const PON_REORG_START_HEIGHT: i64 = 2_020_000;
pub const PON_REORG_END_HEIGHT: i64 = 2_025_000;

pub fn max_reorg_depth(height: i64) -> i64 {
    if (PON_REORG_START_HEIGHT..=PON_REORG_END_HEIGHT).contains(&height) {
        PON_REORG_LENGTH
    } else {
        MAX_REORG_LENGTH
    }
}
/// The minimum allowed transaction version (network rule).
pub const SPROUT_MIN_TX_VERSION: i32 = 1;
/// The minimum allowed Overwinter transaction version (network rule).
pub const OVERWINTER_MIN_TX_VERSION: i32 = 3;
/// The maximum allowed Overwinter transaction version (network rule).
pub const OVERWINTER_MAX_TX_VERSION: i32 = 3;
/// The minimum allowed Sapling transaction version (network rule).
pub const SAPLING_MIN_TX_VERSION: i32 = 4;
/// The maximum allowed Sapling transaction version (network rule).
pub const SAPLING_MAX_TX_VERSION: i32 = 4;
/// The maximum allowed size for a serialized block, in bytes (network rule).
pub const MAX_BLOCK_SIZE: u32 = 2_000_000;
/// The maximum allowed number of signature check operations in a block (network rule).
pub const MAX_BLOCK_SIGOPS: u32 = 20_000;
/// The maximum size of a transaction before Sapling (network rule).
pub const MAX_TX_SIZE_BEFORE_SAPLING: u32 = 100_000;
/// The maximum size of a transaction after Sapling (network rule).
pub const MAX_TX_SIZE_AFTER_SAPLING: u32 = MAX_BLOCK_SIZE;
/// Coinbase transaction outputs can only be spent after this number of new blocks.
pub const COINBASE_MATURITY: i32 = 100;
/// The minimum value which is invalid for expiry height.
pub const TX_EXPIRY_HEIGHT_THRESHOLD: u32 = 500_000_000;

/// Use GetMedianTimePast() instead of nTime for end point timestamp.
pub const LOCKTIME_MEDIAN_TIME_PAST: u32 = 1 << 1;
/// Standard locktime verify flags used by non-consensus code.
pub const STANDARD_LOCKTIME_VERIFY_FLAGS: u32 = LOCKTIME_MEDIAN_TIME_PAST;

/// Current network protocol version for P2P messages.
pub const PROTOCOL_VERSION: i32 = 170_020;
