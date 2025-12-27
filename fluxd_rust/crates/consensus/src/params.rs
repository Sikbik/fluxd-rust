//! Consensus parameter definitions.

use crate::money::{Amount, COIN};
use crate::upgrades::{Hash256, NetworkUpgrade, MAX_NETWORK_UPGRADES};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Network {
    Mainnet,
    Testnet,
    Regtest,
}

#[derive(Clone, Debug)]
pub struct ConsensusParams {
    pub network: Network,
    pub hash_genesis_block: Hash256,
    pub genesis_time: u32,
    pub coinbase_must_be_protected: bool,
    pub subsidy_slow_start_interval: i32,
    pub subsidy_halving_interval: i32,
    pub majority_enforce_block_upgrade: i32,
    pub majority_reject_block_outdated: i32,
    pub majority_window: i32,
    pub upgrades: [NetworkUpgrade; MAX_NETWORK_UPGRADES],
    pub emergency: EmergencyParams,
    pub checkpoints: Vec<Checkpoint>,
    pub pow_limit: Hash256,
    pub pon_limit: Hash256,
    pub pon_start_limit: Hash256,
    pub pow_allow_min_difficulty_after_height: Option<i32>,
    pub digishield_averaging_window: i64,
    pub digishield_max_adjust_down: i64,
    pub digishield_max_adjust_up: i64,
    pub pow_target_spacing: i64,
    pub pon_target_spacing: i64,
    pub pon_difficulty_window: i64,
    pub pon_subsidy_reduction_interval: i32,
    pub pon_max_reductions: i32,
    pub pon_initial_subsidy: i32,
    pub minimum_chain_work: Hash256,
    pub zawy_lwma_averaging_window: i64,
    pub eh_epoch_fade_length: u64,
    pub eh_epoch_1: EquihashParams,
    pub eh_epoch_2: EquihashParams,
    pub eh_epoch_3: EquihashParams,
}

impl ConsensusParams {
    pub fn subsidy_slow_start_shift(&self) -> i32 {
        self.subsidy_slow_start_interval / 2
    }

    pub fn last_founders_reward_block_height(&self) -> i32 {
        -1
    }

    pub fn digishield_averaging_window_timespan(&self) -> i64 {
        self.digishield_averaging_window * self.pow_target_spacing
    }

    pub fn digishield_min_actual_timespan(&self) -> i64 {
        (self.digishield_averaging_window_timespan() * (100 - self.digishield_max_adjust_up)) / 100
    }

    pub fn digishield_max_actual_timespan(&self) -> i64 {
        (self.digishield_averaging_window_timespan() * (100 + self.digishield_max_adjust_down))
            / 100
    }
}

#[derive(Debug)]
pub enum HexError {
    InvalidLength,
    InvalidHex,
}

pub fn hash256_from_hex(input: &str) -> Result<Hash256, HexError> {
    let mut hex = input.trim();
    if let Some(stripped) = hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X")) {
        hex = stripped;
    }

    if hex.is_empty() {
        return Err(HexError::InvalidLength);
    }

    let mut hex_owned = if hex.len() % 2 == 1 {
        let mut padded = String::with_capacity(hex.len() + 1);
        padded.push('0');
        padded.push_str(hex);
        padded
    } else {
        hex.to_string()
    };

    if hex_owned.len() > 64 {
        return Err(HexError::InvalidLength);
    }

    if hex_owned.len() < 64 {
        let mut padded = String::with_capacity(64);
        for _ in 0..(64 - hex_owned.len()) {
            padded.push('0');
        }
        padded.push_str(&hex_owned);
        hex_owned = padded;
    }

    let mut bytes = [0u8; 32];
    for (i, byte_out) in bytes.iter_mut().enumerate() {
        let start = i * 2;
        let byte = u8::from_str_radix(&hex_owned[start..start + 2], 16)
            .map_err(|_| HexError::InvalidHex)?;
        *byte_out = byte;
    }
    bytes.reverse();

    Ok(bytes)
}

pub fn consensus_params(network: Network) -> ConsensusParams {
    match network {
        Network::Mainnet => mainnet_consensus_params(),
        Network::Testnet => testnet_consensus_params(),
        Network::Regtest => regtest_consensus_params(),
    }
}

#[derive(Clone, Debug)]
pub struct FundingParams {
    pub exchange_address: &'static str,
    pub exchange_height: i64,
    pub exchange_amount: Amount,
    pub foundation_address: &'static str,
    pub foundation_height: i64,
    pub foundation_amount: Amount,
    pub dev_fund_address: &'static str,
}

#[derive(Clone, Debug)]
pub struct SwapPoolParams {
    pub address: &'static str,
    pub start_height: i64,
    pub amount: Amount,
    pub interval: i64,
    pub max_times: i32,
}

#[derive(Clone, Debug)]
pub struct EmergencyParams {
    pub public_keys: &'static [&'static str],
    pub collateral_hash: Hash256,
    pub min_signatures: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub height: i32,
    pub hash: Hash256,
}

#[derive(Clone, Copy, Debug)]
pub struct TimedPublicKey {
    pub key: &'static str,
    pub valid_from: u32,
}

#[derive(Clone, Debug)]
pub struct FluxnodeParams {
    pub start_payments_height: i64,
    pub benchmarking_public_keys: &'static [TimedPublicKey],
    pub p2sh_public_keys: &'static [TimedPublicKey],
    pub cumulus_transition_start: i64,
    pub cumulus_transition_end: i64,
    pub nimbus_transition_start: i64,
    pub nimbus_transition_end: i64,
    pub stratus_transition_start: i64,
    pub stratus_transition_end: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EquihashParams {
    pub n: u8,
    pub k: u8,
    pub solution_size: u16,
}

#[derive(Clone, Debug)]
pub struct ChainParams {
    pub network: Network,
    pub consensus: ConsensusParams,
    pub funding: FundingParams,
    pub swap_pool: SwapPoolParams,
    pub emergency: EmergencyParams,
    pub fluxnode: FluxnodeParams,
    pub message_start: [u8; 4],
    pub default_port: u16,
    pub dns_seeds: &'static [&'static str],
    pub fixed_seeds: &'static [&'static str],
}

pub fn chain_params(network: Network) -> ChainParams {
    match network {
        Network::Mainnet => mainnet_chain_params(),
        Network::Testnet => testnet_chain_params(),
        Network::Regtest => regtest_chain_params(),
    }
}

fn mainnet_consensus_params() -> ConsensusParams {
    let eh_200_9 = EquihashParams {
        n: 200,
        k: 9,
        solution_size: 1344,
    };
    let eh_144_5 = EquihashParams {
        n: 144,
        k: 5,
        solution_size: 100,
    };
    let eh_zelhash = EquihashParams {
        n: 125,
        k: 4,
        solution_size: 52,
    };
    let upgrades = [
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: NetworkUpgrade::ALWAYS_ACTIVE,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: 125_000,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: 125_100,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_007,
            activation_height: 250_000,
            hash_activation_block: Some(
                hash256_from_hex(
                    "0000001d65fa78f2f6c172a51b5aca59ee1927e51f728647fca21b180becfe59",
                )
                .expect("mainnet acadia activation hash"),
            ),
        },
        NetworkUpgrade {
            protocol_version: 170_012,
            activation_height: 372_500,
            hash_activation_block: Some(
                hash256_from_hex(
                    "00000052e2ac144c2872ff641c646e41dac166ac577bc9b0837f501aba19de4a",
                )
                .expect("mainnet kamiooka activation hash"),
            ),
        },
        NetworkUpgrade {
            protocol_version: 170_016,
            activation_height: 558_000,
            hash_activation_block: Some(
                hash256_from_hex(
                    "000000a33d38f37f586b843a9c8cf6d1ff1269e6114b34604cabcd14c44268d4",
                )
                .expect("mainnet kamata activation hash"),
            ),
        },
        NetworkUpgrade {
            protocol_version: 170_017,
            activation_height: 835_554,
            hash_activation_block: Some(
                hash256_from_hex(
                    "000000ce99aa6765bdaae673cdf41f661ff20a116eb6f2fe0843488d8061f193",
                )
                .expect("mainnet flux activation hash"),
            ),
        },
        NetworkUpgrade {
            protocol_version: 170_018,
            activation_height: 1_076_532,
            hash_activation_block: Some(
                hash256_from_hex(
                    "000000111f8643ce24d9753dbc324220877299075a8a6102da61ef4460296325",
                )
                .expect("mainnet halving activation hash"),
            ),
        },
        NetworkUpgrade {
            protocol_version: 170_019,
            activation_height: 1_549_500,
            hash_activation_block: Some(
                hash256_from_hex(
                    "00000009f9178347f3dea495a089400050c3388e07f9c871fb1ebddcab1f8044",
                )
                .expect("mainnet p2shnodes activation hash"),
            ),
        },
        NetworkUpgrade {
            protocol_version: 170_020,
            activation_height: 2_020_000,
            hash_activation_block: None,
        },
    ];

    ConsensusParams {
        network: Network::Mainnet,
        hash_genesis_block: hash256_from_hex(
            "00052461a5006c2e3b74ce48992a08695607912d5604c3eb8da25749b0900444",
        )
        .expect("mainnet genesis hash"),
        genesis_time: 1_516_980_000,
        coinbase_must_be_protected: true,
        subsidy_slow_start_interval: 5_000,
        subsidy_halving_interval: 655_350,
        majority_enforce_block_upgrade: 750,
        majority_reject_block_outdated: 950,
        majority_window: 4_000,
        upgrades,
        emergency: mainnet_emergency_params(),
        checkpoints: mainnet_checkpoints(),
        pow_limit: hash256_from_hex(
            "0007ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("mainnet pow limit"),
        pon_limit: hash256_from_hex(
            "0fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("mainnet pon limit"),
        pon_start_limit: hash256_from_hex(
            "000bffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("mainnet pon start limit"),
        pow_allow_min_difficulty_after_height: None,
        digishield_averaging_window: 17,
        digishield_max_adjust_down: 32,
        digishield_max_adjust_up: 16,
        pow_target_spacing: 120,
        pon_target_spacing: 30,
        pon_difficulty_window: 30,
        pon_subsidy_reduction_interval: 1_051_200,
        pon_max_reductions: 20,
        pon_initial_subsidy: 14,
        minimum_chain_work: hash256_from_hex(
            "000000000000000000000000000000000000000000000000000021f5d5da5d73",
        )
        .expect("mainnet minimum chain work"),
        zawy_lwma_averaging_window: 60,
        eh_epoch_fade_length: 11,
        eh_epoch_1: eh_200_9,
        eh_epoch_2: eh_144_5,
        eh_epoch_3: eh_zelhash,
    }
}

fn testnet_consensus_params() -> ConsensusParams {
    let eh_48_5 = EquihashParams {
        n: 48,
        k: 5,
        solution_size: 36,
    };
    let upgrades = [
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: NetworkUpgrade::ALWAYS_ACTIVE,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: 70,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: 140,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_007,
            activation_height: 210,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_012,
            activation_height: 280,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_016,
            activation_height: 350,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_017,
            activation_height: 420,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_018,
            activation_height: 520,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_019,
            activation_height: 600,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_020,
            activation_height: 800,
            hash_activation_block: None,
        },
    ];

    ConsensusParams {
        network: Network::Testnet,
        hash_genesis_block: hash256_from_hex(
            "0042202a64a929fc25cc10e68615ddbe38007b1b40da08acd3f530f83c79b9d1",
        )
        .expect("testnet genesis hash"),
        genesis_time: 1_582_228_940,
        coinbase_must_be_protected: true,
        subsidy_slow_start_interval: 1,
        subsidy_halving_interval: 655_350,
        majority_enforce_block_upgrade: 51,
        majority_reject_block_outdated: 75,
        majority_window: 400,
        upgrades,
        emergency: testnet_emergency_params(),
        checkpoints: testnet_checkpoints(),
        pow_limit: hash256_from_hex(
            "0effffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("testnet pow limit"),
        pon_limit: hash256_from_hex(
            "0fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("testnet pon limit"),
        pon_start_limit: hash256_from_hex(
            "7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("testnet pon start limit"),
        pow_allow_min_difficulty_after_height: Some(0),
        digishield_averaging_window: 17,
        digishield_max_adjust_down: 32,
        digishield_max_adjust_up: 16,
        pow_target_spacing: 60,
        pon_target_spacing: 30,
        pon_difficulty_window: 60,
        pon_subsidy_reduction_interval: 525_600,
        pon_max_reductions: 20,
        pon_initial_subsidy: 14,
        minimum_chain_work: [0u8; 32],
        zawy_lwma_averaging_window: 60,
        eh_epoch_fade_length: 10,
        eh_epoch_1: eh_48_5,
        eh_epoch_2: eh_48_5,
        eh_epoch_3: eh_48_5,
    }
}

fn regtest_consensus_params() -> ConsensusParams {
    let eh_200_9 = EquihashParams {
        n: 200,
        k: 9,
        solution_size: 1344,
    };
    let eh_144_5 = EquihashParams {
        n: 144,
        k: 5,
        solution_size: 100,
    };
    let eh_zelhash = EquihashParams {
        n: 125,
        k: 4,
        solution_size: 52,
    };
    let upgrades = [
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: NetworkUpgrade::ALWAYS_ACTIVE,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_002,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_006,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_012,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_016,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_017,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_018,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_019,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
        NetworkUpgrade {
            protocol_version: 170_020,
            activation_height: NetworkUpgrade::NO_ACTIVATION_HEIGHT,
            hash_activation_block: None,
        },
    ];

    ConsensusParams {
        network: Network::Regtest,
        hash_genesis_block: hash256_from_hex(
            "01998760a88dc2b5715f69d2f18c1d90e0b604612242d9099eaff3048dd1e0ce",
        )
        .expect("regtest genesis hash"),
        genesis_time: 1_296_688_602,
        coinbase_must_be_protected: false,
        subsidy_slow_start_interval: 0,
        subsidy_halving_interval: 150,
        majority_enforce_block_upgrade: 750,
        majority_reject_block_outdated: 950,
        majority_window: 1_000,
        upgrades,
        emergency: regtest_emergency_params(),
        checkpoints: regtest_checkpoints(),
        pow_limit: hash256_from_hex(
            "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f",
        )
        .expect("regtest pow limit"),
        pon_limit: hash256_from_hex(
            "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f",
        )
        .expect("regtest pon limit"),
        pon_start_limit: hash256_from_hex(
            "7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .expect("regtest pon start limit"),
        pow_allow_min_difficulty_after_height: Some(0),
        digishield_averaging_window: 17,
        digishield_max_adjust_down: 0,
        digishield_max_adjust_up: 0,
        pow_target_spacing: 120,
        pon_target_spacing: 30,
        pon_difficulty_window: 60,
        pon_subsidy_reduction_interval: 100,
        pon_max_reductions: 10,
        pon_initial_subsidy: 14,
        minimum_chain_work: hash256_from_hex("00").expect("regtest minimum chain work"),
        zawy_lwma_averaging_window: 60,
        eh_epoch_fade_length: 11,
        eh_epoch_1: eh_200_9,
        eh_epoch_2: eh_144_5,
        eh_epoch_3: eh_zelhash,
    }
}

const MAINNET_EMERGENCY_KEYS: [&str; 4] = [
    "025ee73f72d6996f94fe6ec9fac3f9ba6dcb947ed46dfbda530fc73ff99c667a4e",
    "026f4281124d10eb90589831bac405d715ad79051ac5243d21c322d2abf2fd81e2",
    "03083d65c2f57cfe4d1c34eb575bd9d836f5111dd0de86405d48211bf42ea30403",
    "03674c29f348124e998fd838228a3ff050ca26fe0c13ad98698585cbbf796b461e",
];

const TESTNET_EMERGENCY_KEYS: [&str; 4] = [
    "029a1c55fa7e69dd99087f7ca799797052ae21327b94159e60b8cc5704eb188583",
    "023c806b01f35a18b42b08f23f5c7e8490801a7da8fd6f6e77708d9f26f22c423e",
    "02615c78e21078c21a63cb21cc4d29eaa148d97c3dcbe7be5d9d4dda4e969bb05a",
    "033d301dc7ef7ab653da36da1285063ea9be1448d601e3c9f99185476b9d4ae1d1",
];

const MAINNET_BENCHMARKING_KEYS: [TimedPublicKey; 5] = [
    TimedPublicKey {
        key: "042e79d7dd1483996157df6b16c831be2b14b31c69944ea2a585c63b5101af1f9517ba392cee5b1f45a62e9d936488429374535a2f76870bfa8eea6667b13eb39e",
        valid_from: 0,
    },
    TimedPublicKey {
        key: "04517413e51fa9b2e94f200b254cca69beb86f2d74bf66ca53854ba66bc376dde9b52e9b4403731d9a4f3e8edd9687f1e1824b688fe26454bd9fb823a3307b4682",
        valid_from: 1_618_113_600,
    },
    TimedPublicKey {
        key: "0480dff65aa9d4b4c4234e4723a5e7c5bf527ca683b53aa26a7225cc5eb16e6e79f9629eb5f96c12b173de7a20e9823b2d36575759f3490864922f7ed04e171fad",
        valid_from: 1_647_262_800,
    },
    TimedPublicKey {
        key: "0437d58236a849ebe0e6558c1517e1f5c56749e04a2f7a7daedd4ef7c9fb6a773f32a33fe5ddad88b9af3ff496ee5ce79ce245c258bafa4e8d287baa3d54c6c65f",
        valid_from: 1_706_209_200,
    },
    TimedPublicKey {
        key: "04e54965119e89861f80135b13f56c3b5cc55ad2b916d9705052fc91d2894f9cb19151a8d61c6e9ea4812075dd32f06fde5965589ffa1517ab1bd2ddbc66a39f42",
        valid_from: 1_743_534_000,
    },
];

const TESTNET_BENCHMARKING_KEYS: [TimedPublicKey; 2] = [
    TimedPublicKey {
        key: "04d422e01f5acff68504b92df96a9004cf61be432a20efe83fe8a94c1aa730fe7dece5d2e8298f2d5672d4e569c55d9f0a73268ef7b92990d8c014e828a7cc48dd",
        valid_from: 0,
    },
    TimedPublicKey {
        key: "042023568fbcc4715c34d8596feaabf0683b3dfa7280b2f4df0436311a31086a73fdf507d63c3ec89455037ba738375d17b309c2cd226f173a5ef7841400cd09ec",
        valid_from: 1_617_508_800,
    },
];

const REGTEST_BENCHMARKING_KEYS: [TimedPublicKey; 2] = [
    TimedPublicKey {
        key: "04cf3c34f01486bbb34c1a7ca11c2ddb1b3d98698c3f37d54452ff91a8cd5e92a6910ce5fc2cc7ad63547454a965df53ff5be740d4ef4ac89848c2bafd1e40e6b7",
        valid_from: 0,
    },
    TimedPublicKey {
        key: "045d54130187b4c4bba25004bf615881c2d79b16950a59114df27dc9858d8e531fda4f3a27aa95ceb2bcc87ddd734be40a6808422655e5350fa9417874556b7342",
        valid_from: 1_617_508_800,
    },
];

const MAINNET_P2SH_KEYS: [TimedPublicKey; 1] = [TimedPublicKey {
    key: "04ab11edbb8a15f7cc2628a4a2c18cea095d250f8c9a2924cbd581b8d8fb3a8b91e39e5febddb7ffc60f20dfd352a40aa4f061aa60a9ace26d43e1b7a18aea4162",
    valid_from: 0,
}];

const TESTNET_P2SH_KEYS: [TimedPublicKey; 1] = [TimedPublicKey {
    key: "04276f105ff36a670a56e75c2462cff05a4a7864756e6e1af01022e32752d6fe57b1e13cab4f2dbe3a6a51b4e0de83a5c4627345f5232151867850018c9a3c3a1d",
    valid_from: 0,
}];

const REGTEST_P2SH_KEYS: [TimedPublicKey; 1] = [TimedPublicKey {
    key: "04276f105ff36a670a56e75c2462cff05a4a7864756e6e1af01022e32752d6fe57b1e13cab4f2dbe3a6a51b4e0de83a5c4627345f5232151867850018c9a3c3a1d",
    valid_from: 0,
}];

const MAINNET_DNS_SEEDS: [&str; 3] = [
    "dnsseed.asoftwaresolution.com",
    "dnsseed.zel.network",
    "dnsseed.runonflux.io",
];

const MAINNET_FIXED_SEEDS: [&str; 11] = [
    "46.36.38.23:16125",
    "46.36.39.93:16125",
    "45.63.86.148:16125",
    "45.63.83.125:16125",
    "52.171.140.27:16125",
    "142.44.143.182:16125",
    "173.212.207.13:16125",
    "173.249.13.224:16125",
    "136.33.111.57:16125",
    "35.194.136.53:16125",
    "35.205.124.144:16125",
];

const TESTNET_DNS_SEEDS: [&str; 3] = [
    "flux-testnet-seed.asoftwaresolution.com",
    "test.dnsseed.zel.network",
    "test.dnsseed.runonflux.io",
];

const TESTNET_FIXED_SEEDS: [&str; 0] = [];

const REGTEST_DNS_SEEDS: [&str; 0] = [];
const REGTEST_FIXED_SEEDS: [&str; 0] = [];

fn mainnet_emergency_params() -> EmergencyParams {
    EmergencyParams {
        public_keys: &MAINNET_EMERGENCY_KEYS,
        collateral_hash: hash256_from_hex(
            "1111111111111111111111111111111111111111111111111111111111111111",
        )
        .expect("mainnet emergency collateral hash"),
        min_signatures: 2,
    }
}

fn testnet_emergency_params() -> EmergencyParams {
    EmergencyParams {
        public_keys: &TESTNET_EMERGENCY_KEYS,
        collateral_hash: hash256_from_hex(
            "1111111111111111111111111111111111111111111111111111111111111111",
        )
        .expect("testnet emergency collateral hash"),
        min_signatures: 2,
    }
}

fn regtest_emergency_params() -> EmergencyParams {
    EmergencyParams {
        public_keys: &TESTNET_EMERGENCY_KEYS,
        collateral_hash: hash256_from_hex(
            "1111111111111111111111111111111111111111111111111111111111111111",
        )
        .expect("regtest emergency collateral hash"),
        min_signatures: 1,
    }
}

fn parse_checkpoints(entries: &[(i32, &str)]) -> Vec<Checkpoint> {
    entries
        .iter()
        .map(|(height, hash)| Checkpoint {
            height: *height,
            hash: hash256_from_hex(hash).expect("checkpoint hash"),
        })
        .collect()
}

fn mainnet_checkpoints() -> Vec<Checkpoint> {
    parse_checkpoints(&[
        (
            0,
            "00052461a5006c2e3b74ce48992a08695607912d5604c3eb8da25749b0900444",
        ),
        (
            5500,
            "0000000e7724f8bace09dd762657169c10622af4a6a8e959152cd00b9119848e",
        ),
        (
            35000,
            "000000004646dd797644b9c67aff320961e95c311b4f26985424b720d09fcaa5",
        ),
        (
            70000,
            "00000001edcf7768ed39fac55414e53a78d077b1b41fccdaf9307d7bc219626a",
        ),
        (
            94071,
            "00000005ec83876bc5288badf0971ae83ac7c6a286851f7b22a75a03e73b401a",
        ),
        (
            277649,
            "00000004a53f9271d05071a052b3738b46663f3335d14b6aea965a3cb70c0cc8",
        ),
        (
            400000,
            "000000390342f0e52443ad79b43e5d85b78bf519667aeb3aa980d76caeda0369",
        ),
        (
            530000,
            "0000004b4459ec6904e8116d178c357b0f25a7d45c5c5836ce3714791f1ed124",
        ),
        (
            600000,
            "000000dea4478401e6ab95f6d05ade810115411e95e75fab9fd94a44df4b1e1d",
        ),
        (
            700000,
            "0000000845ef03939225cc592773fd7aef54b5232fc42790c46ef6f11ee3e8d4",
        ),
        (
            800000,
            "000000451b73f495b2f6ad38bd89d15495551fc15c2078ad7af3d54d06422cc6",
        ),
        (
            900000,
            "000001e1ad2bb5e3cabb09559b6e65b871bf1d2a51bcc141ce45fc4cbd1d9cd8",
        ),
        (
            1000000,
            "0000001a80e7f30d21fb14116cd01d51e1fad8ac84cc960896f4691a57368a47",
        ),
        (
            1040000,
            "00000007f3b465bd4b0e161e43c05a3d946144330e33ea3a91cb952e6ef86b7d",
        ),
        (
            1040577,
            "000000071fe89682ac260bc0a49621344eb28ae01659c9e7ce86e3762e45f52d",
        ),
        (
            1042126,
            "0000000295e4663178fd9e533787e74206645910a2bfb61938db5f67796eaad0",
        ),
        (
            1060000,
            "0000000fd721d8d381c4b24a4f78fc036955d7a0f98d2765b8c7badad8b66c1b",
        ),
        (
            1442798,
            "0000000cc561fecb2ecfd22ba7af09450ca8cf270f407ce8b948195ff2aa0d13",
        ),
        (
            1518503,
            "0000000dba41dc84c52a3933af49d316fff49a76b49d42bd5b6d20c4e451a0ef",
        ),
        (
            1791720,
            "0000000abc7bd62a213e0dab43c9c01220b031a568fdfb5c2ef89e6b30054bdc",
        ),
        (
            2020500,
            "af2a1bd59c61f64860b4b45bd65358743fda40d8420564b58c39df45be7da97c",
        ),
        (
            2021000,
            "d2dcec473e809575e30ec2c0f400758120f5121b8268f90cdb8a7dbefe285b0d",
        ),
        (
            2021500,
            "fa98471f31ffc1366330bababc090ad5cb6bd23c25bb3b61d1e1ed07a77d6126",
        ),
        (
            2022000,
            "40a060546a56eb7fab0fd33ab3e6de834ff0d5273847d4f231a9addecfc44f61",
        ),
        (
            2029000,
            "4856dc788a973db4cc537465c9ef80288e1eb065898993d72371b1ee48c248b4",
        ),
    ])
}

fn testnet_checkpoints() -> Vec<Checkpoint> {
    parse_checkpoints(&[
        (
            0,
            "0042202a64a929fc25cc10e68615ddbe38007b1b40da08acd3f530f83c79b9d1",
        ),
        (
            320,
            "0237bf16aba912b0c68933809a7e7fe9553ddff1bc0782d2463fc5d161af1c46",
        ),
    ])
}

fn regtest_checkpoints() -> Vec<Checkpoint> {
    parse_checkpoints(&[(
        0,
        "01998760a88dc2b5715f69d2f18c1d90e0b604612242d9099eaff3048dd1e0ce",
    )])
}

fn mainnet_chain_params() -> ChainParams {
    ChainParams {
        network: Network::Mainnet,
        consensus: mainnet_consensus_params(),
        funding: FundingParams {
            exchange_address: "t3PMbbA5YBMrjSD3dD16SSdXKuKovwmj6tS",
            exchange_height: 836_274,
            exchange_amount: 7_500_000 * COIN,
            foundation_address: "t3XjYMBvwxnXVv9jqg4CgokZ3f7kAoXPQL8",
            foundation_height: 836_994,
            foundation_amount: 2_500_000 * COIN,
            dev_fund_address: "t3hPu1YDeGUCp8m7BQCnnNUmRMJBa5RadyA",
        },
        swap_pool: SwapPoolParams {
            address: "t3ThbWogDoAjGuS6DEnmN1GWJBRbVjSUK4T",
            start_height: 837_714,
            amount: 22_000_000 * COIN,
            interval: 21_600,
            max_times: 10,
        },
        emergency: mainnet_emergency_params(),
        fluxnode: FluxnodeParams {
            start_payments_height: 560_000,
            benchmarking_public_keys: &MAINNET_BENCHMARKING_KEYS,
            p2sh_public_keys: &MAINNET_P2SH_KEYS,
            cumulus_transition_start: 1_076_532,
            cumulus_transition_end: 1_086_612,
            nimbus_transition_start: 1_081_572,
            nimbus_transition_end: 1_092_372,
            stratus_transition_start: 1_087_332,
            stratus_transition_end: 1_097_412,
        },
        message_start: [0x24, 0xe9, 0x27, 0x64],
        default_port: 16_125,
        dns_seeds: &MAINNET_DNS_SEEDS,
        fixed_seeds: &MAINNET_FIXED_SEEDS,
    }
}

fn testnet_chain_params() -> ChainParams {
    ChainParams {
        network: Network::Testnet,
        consensus: testnet_consensus_params(),
        funding: FundingParams {
            exchange_address: "tmRucHD85zgSigtA4sJJBDbPkMUJDcw5XDE",
            exchange_height: 4_100,
            exchange_amount: 7_500_000 * COIN,
            foundation_address: "tmRucHD85zgSigtA4sJJBDbPkMUJDcw5XDE",
            foundation_height: 4_200,
            foundation_amount: 2_500_000 * COIN,
            dev_fund_address: "t2GoxS2SRmLQDnTyWePHjKD3izvFsKUAjrH",
        },
        swap_pool: SwapPoolParams {
            address: "tmRucHD85zgSigtA4sJJBDbPkMUJDcw5XDE",
            start_height: 4_300,
            amount: 2_200_000 * COIN,
            interval: 100,
            max_times: 10,
        },
        emergency: testnet_emergency_params(),
        fluxnode: FluxnodeParams {
            start_payments_height: 350,
            benchmarking_public_keys: &TESTNET_BENCHMARKING_KEYS,
            p2sh_public_keys: &TESTNET_P2SH_KEYS,
            cumulus_transition_start: 420,
            cumulus_transition_end: 520,
            nimbus_transition_start: 420,
            nimbus_transition_end: 520,
            stratus_transition_start: 420,
            stratus_transition_end: 520,
        },
        message_start: [0xfa, 0x1a, 0xf9, 0xbf],
        default_port: 26_125,
        dns_seeds: &TESTNET_DNS_SEEDS,
        fixed_seeds: &TESTNET_FIXED_SEEDS,
    }
}

fn regtest_chain_params() -> ChainParams {
    ChainParams {
        network: Network::Regtest,
        consensus: regtest_consensus_params(),
        funding: FundingParams {
            exchange_address: "tmRucHD85zgSigtA4sJJBDbPkMUJDcw5XDE",
            exchange_height: 10,
            exchange_amount: 3_000_000 * COIN,
            foundation_address: "t2DFGpj2tciojsGKKrGVwQ92hUwAxWQQgJ9",
            foundation_height: 10,
            foundation_amount: 2_500_000 * COIN,
            dev_fund_address: "t2GoxS2SRmLQDnTyWePHjKD3izvFsKUAjrH",
        },
        swap_pool: SwapPoolParams {
            address: "t2Dsexh4v5g2dpL2LLCsR1p9TshMm63jSBM",
            start_height: 10,
            amount: 2_100_000 * COIN,
            interval: 10,
            max_times: 5,
        },
        emergency: regtest_emergency_params(),
        fluxnode: FluxnodeParams {
            start_payments_height: 100,
            benchmarking_public_keys: &REGTEST_BENCHMARKING_KEYS,
            p2sh_public_keys: &REGTEST_P2SH_KEYS,
            cumulus_transition_start: 0,
            cumulus_transition_end: 1_000,
            nimbus_transition_start: 0,
            nimbus_transition_end: 1_000,
            stratus_transition_start: 0,
            stratus_transition_end: 100,
        },
        message_start: [0xaa, 0xe8, 0x3f, 0x5f],
        default_port: 26_126,
        dns_seeds: &REGTEST_DNS_SEEDS,
        fixed_seeds: &REGTEST_FIXED_SEEDS,
    }
}
