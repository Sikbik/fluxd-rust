//! Chainstate and UTXO/anchor management.

pub mod anchors;
pub mod address_index;
pub mod flatfiles;
pub mod index;
pub mod metrics;
mod shielded;
pub mod state;
pub mod txindex;
pub mod utxo;
pub mod undo;
pub mod validation;
