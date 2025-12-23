//! Core block/transaction types and consensus serialization.

pub mod block;
pub mod address;
pub mod encoding;
pub mod hash;
pub mod outpoint;
pub mod transaction;

pub use address::{address_to_script_pubkey, script_pubkey_to_address, AddressError};
pub use block::{Block, BlockHeader};
pub use hash::{sha256, sha256d};
pub use outpoint::OutPoint;
pub use transaction::{
    JoinSplit, OutputDescription, SpendDescription, SproutProof, Transaction, TransactionDecodeError,
    TransactionEncodeError, TxIn, TxOut,
};
