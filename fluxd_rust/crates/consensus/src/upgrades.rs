//! Network upgrade schedule and branch IDs.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum UpgradeIndex {
    BaseSprout = 0,
    TestDummy = 1,
    Lwma = 2,
    Equi144_5 = 3,
    Acadia = 4,
    Kamiooka = 5,
    Kamata = 6,
    Flux = 7,
    Halving = 8,
    P2ShNodes = 9,
    Pon = 10,
}

pub const MAX_NETWORK_UPGRADES: usize = 11;

pub const ALL_UPGRADES: [UpgradeIndex; MAX_NETWORK_UPGRADES] = [
    UpgradeIndex::BaseSprout,
    UpgradeIndex::TestDummy,
    UpgradeIndex::Lwma,
    UpgradeIndex::Equi144_5,
    UpgradeIndex::Acadia,
    UpgradeIndex::Kamiooka,
    UpgradeIndex::Kamata,
    UpgradeIndex::Flux,
    UpgradeIndex::Halving,
    UpgradeIndex::P2ShNodes,
    UpgradeIndex::Pon,
];

impl UpgradeIndex {
    pub const fn as_usize(self) -> usize {
        self as usize
    }
}

pub type Hash256 = [u8; 32];

#[derive(Clone, Copy, Debug)]
pub struct NetworkUpgrade {
    pub protocol_version: i32,
    pub activation_height: i32,
    pub hash_activation_block: Option<Hash256>,
}

impl NetworkUpgrade {
    pub const ALWAYS_ACTIVE: i32 = 0;
    pub const NO_ACTIVATION_HEIGHT: i32 = -1;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpgradeState {
    Disabled,
    Pending,
    Active,
}

#[derive(Clone, Copy, Debug)]
pub struct UpgradeInfo {
    pub branch_id: u32,
    pub name: &'static str,
    pub info: &'static str,
}

pub const NETWORK_UPGRADE_INFO: [UpgradeInfo; MAX_NETWORK_UPGRADES] = [
    UpgradeInfo {
        branch_id: 0,
        name: "Base",
        info: "The Zelcash network at launch",
    },
    UpgradeInfo {
        branch_id: 0x74736554,
        name: "Test dummy",
        info: "Test dummy info",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "LWMA",
        info: "Zelcash upgraded to LWMA difficulty algorithm",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "Equihash 144/5",
        info: "Zelcash PoW Change to Equihash 144/5",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "Acadia",
        info: "The Zelcash Acadia Update",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "Kamiooka",
        info: "Zel Kamiooka Upgrade, PoW change to ZelHash and update for ZelNodes",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "Kamata",
        info: "Zel Kamata Upgrade, Deterministic ZelNodes and ZelFlux",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "Flux",
        info: "Flux Upgrade, Multiple chains",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "Halving",
        info: "Flux Halving",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "P2SHNodes",
        info: "Multisig Node Upgrade",
    },
    UpgradeInfo {
        branch_id: 0x76b8_09bb,
        name: "PON",
        info: "Proof of Node activation",
    },
];

pub const SPROUT_BRANCH_ID: u32 = NETWORK_UPGRADE_INFO[UpgradeIndex::BaseSprout as usize].branch_id;

pub fn network_upgrade_state(
    height: i32,
    upgrades: &[NetworkUpgrade; MAX_NETWORK_UPGRADES],
    idx: UpgradeIndex,
) -> UpgradeState {
    let activation_height = upgrades[idx.as_usize()].activation_height;
    if activation_height == NetworkUpgrade::NO_ACTIVATION_HEIGHT {
        UpgradeState::Disabled
    } else if height >= activation_height {
        UpgradeState::Active
    } else {
        UpgradeState::Pending
    }
}

pub fn network_upgrade_active(
    height: i32,
    upgrades: &[NetworkUpgrade; MAX_NETWORK_UPGRADES],
    idx: UpgradeIndex,
) -> bool {
    network_upgrade_state(height, upgrades, idx) == UpgradeState::Active
}

pub fn current_epoch(height: i32, upgrades: &[NetworkUpgrade; MAX_NETWORK_UPGRADES]) -> UpgradeIndex {
    for idx in ALL_UPGRADES.iter().rev() {
        if network_upgrade_active(height, upgrades, *idx) {
            return *idx;
        }
    }
    UpgradeIndex::BaseSprout
}

pub fn current_epoch_branch_id(height: i32, upgrades: &[NetworkUpgrade; MAX_NETWORK_UPGRADES]) -> u32 {
    let idx = current_epoch(height, upgrades);
    NETWORK_UPGRADE_INFO[idx.as_usize()].branch_id
}

pub fn is_consensus_branch_id(branch_id: u32) -> bool {
    NETWORK_UPGRADE_INFO
        .iter()
        .any(|info| info.branch_id == branch_id)
}

pub fn is_activation_height(
    height: i32,
    upgrades: &[NetworkUpgrade; MAX_NETWORK_UPGRADES],
    idx: UpgradeIndex,
) -> bool {
    if idx == UpgradeIndex::BaseSprout || height < 0 {
        return false;
    }
    height == upgrades[idx.as_usize()].activation_height
}

pub fn is_activation_height_for_any_upgrade(
    height: i32,
    upgrades: &[NetworkUpgrade; MAX_NETWORK_UPGRADES],
) -> bool {
    if height < 0 {
        return false;
    }
    ALL_UPGRADES
        .iter()
        .skip(1)
        .any(|idx| height == upgrades[idx.as_usize()].activation_height)
}

pub fn next_epoch(
    height: i32,
    upgrades: &[NetworkUpgrade; MAX_NETWORK_UPGRADES],
) -> Option<UpgradeIndex> {
    if height < 0 {
        return None;
    }
    for idx in ALL_UPGRADES.iter().skip(1) {
        if network_upgrade_state(height, upgrades, *idx) == UpgradeState::Pending {
            return Some(*idx);
        }
    }
    None
}

pub fn next_activation_height(
    height: i32,
    upgrades: &[NetworkUpgrade; MAX_NETWORK_UPGRADES],
) -> Option<i32> {
    next_epoch(height, upgrades).map(|idx| upgrades[idx.as_usize()].activation_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{consensus_params, Network};

    #[test]
    fn mainnet_activation_edges() {
        let params = consensus_params(Network::Mainnet);

        assert!(!network_upgrade_active(
            124_999,
            &params.upgrades,
            UpgradeIndex::Lwma
        ));
        assert!(network_upgrade_active(
            125_000,
            &params.upgrades,
            UpgradeIndex::Lwma
        ));

        assert!(!network_upgrade_active(
            125_099,
            &params.upgrades,
            UpgradeIndex::Equi144_5
        ));
        assert!(network_upgrade_active(
            125_100,
            &params.upgrades,
            UpgradeIndex::Equi144_5
        ));

        assert!(!network_upgrade_active(
            2_019_999,
            &params.upgrades,
            UpgradeIndex::Pon
        ));
        assert!(network_upgrade_active(
            2_020_000,
            &params.upgrades,
            UpgradeIndex::Pon
        ));
    }

    #[test]
    fn branch_id_selection() {
        let params = consensus_params(Network::Mainnet);

        assert_eq!(current_epoch_branch_id(0, &params.upgrades), SPROUT_BRANCH_ID);

        let lwma_branch = NETWORK_UPGRADE_INFO[UpgradeIndex::Lwma as usize].branch_id;
        assert_eq!(current_epoch_branch_id(125_000, &params.upgrades), lwma_branch);
    }

    #[test]
    fn next_activation_height_tracking() {
        let params = consensus_params(Network::Mainnet);
        assert_eq!(next_activation_height(0, &params.upgrades), Some(125_000));
        assert_eq!(
            next_activation_height(125_000, &params.upgrades),
            Some(125_100)
        );
    }
}
