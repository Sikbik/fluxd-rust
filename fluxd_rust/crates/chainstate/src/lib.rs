//! Chainstate and UTXO/anchor management.

pub mod address_index;
pub mod anchors;
pub mod blockindex;
mod filemeta;
pub mod flatfiles;
pub mod index;
pub mod metrics;
mod shielded;
pub mod state;
pub mod txindex;
pub mod undo;
pub mod utxo;
pub mod validation;
