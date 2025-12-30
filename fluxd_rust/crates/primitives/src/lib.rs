//! Core block/transaction types and consensus serialization.

pub mod address;
pub mod block;
pub mod encoding;
pub mod hash;
pub mod merkleblock;
pub mod outpoint;
pub mod transaction;

pub use address::{
    address_to_script_pubkey, script_pubkey_to_address, secret_key_to_wif, AddressError,
};
pub use block::{Block, BlockHeader};
pub use hash::{sha256, sha256d};
pub use merkleblock::{MerkleBlock, PartialMerkleTree};
pub use outpoint::OutPoint;
pub use transaction::{
    JoinSplit, OutputDescription, SpendDescription, SproutProof, Transaction,
    TransactionDecodeError, TransactionEncodeError, TxIn, TxOut,
};
