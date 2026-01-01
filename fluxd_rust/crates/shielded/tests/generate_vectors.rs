use std::path::{Path, PathBuf};

use fluxd_consensus::params::Network as FluxNetwork;
use fluxd_primitives::transaction::Transaction as FluxTransaction;
use fluxd_shielded::{default_params_dir, load_params, verify_transaction};
use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use sapling_crypto::{value::NoteValue, Node, Rseed};
use transparent::builder::TransparentSigningSet;
use zcash_primitives::transaction::builder::{BuildConfig, Builder};
use zcash_primitives::transaction::fees::transparent::InputSize;
use zcash_primitives::transaction::fees::FeeRule;
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::consensus::{self, NetworkUpgrade, Parameters, TEST_NETWORK};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;

fn pick_param(params_dir: &Path, primary: &str, fallback: &str) -> PathBuf {
    let primary_path = params_dir.join(primary);
    if primary_path.exists() {
        return primary_path;
    }
    let fallback_path = params_dir.join(fallback);
    if fallback_path.exists() {
        return fallback_path;
    }
    primary_path
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

struct ZeroFeeRule;

impl FeeRule for ZeroFeeRule {
    type Error = core::convert::Infallible;

    fn fee_required<P: consensus::Parameters>(
        &self,
        _params: &P,
        _target_height: consensus::BlockHeight,
        _transparent_input_sizes: impl IntoIterator<Item = InputSize>,
        _transparent_output_sizes: impl IntoIterator<Item = usize>,
        _sapling_input_count: usize,
        _sapling_output_count: usize,
        _orchard_action_count: usize,
    ) -> Result<Zatoshis, Self::Error> {
        Ok(Zatoshis::ZERO)
    }
}

#[test]
#[ignore = "manual utility: prints a valid Sapling tx for shielded proof verification vectors"]
fn generate_valid_sapling_tx_vector() {
    let params_dir = std::env::var_os("FLUXD_PARAMS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_params_dir);

    let spend_path = pick_param(
        &params_dir,
        "sapling-spend.params",
        "sapling-spend-testnet.params",
    );
    let output_path = pick_param(
        &params_dir,
        "sapling-output.params",
        "sapling-output-testnet.params",
    );

    let tx_prover = LocalTxProver::new(&spend_path, &output_path);
    let rng = ChaCha20Rng::from_seed([0u8; 32]);

    let extsk = sapling_crypto::zip32::ExtendedSpendingKey::master(&[0u8; 32]);
    let dfvk = extsk.to_diversifiable_full_viewing_key();
    let to = dfvk.default_address().1;

    let note = to.create_note(
        NoteValue::from_raw(50_000),
        Rseed::BeforeZip212(jubjub::Fr::from(1)),
    );
    let cmu = Node::from_cmu(&note.cmu());
    let mut tree = CommitmentTree::<Node, 32>::empty();
    tree.append(cmu).expect("append note commitment");
    let witness = IncrementalWitness::from_tree(tree).expect("make witness");

    let tx_height = TEST_NETWORK
        .activation_height(NetworkUpgrade::Sapling)
        .expect("sapling activation height");
    let build_config = BuildConfig::Standard {
        sapling_anchor: Some(witness.root().into()),
        orchard_anchor: None,
    };

    let mut builder = Builder::new(TEST_NETWORK, tx_height, build_config);
    builder
        .add_sapling_spend::<core::convert::Infallible>(
            dfvk.fvk().clone(),
            note,
            witness.path().expect("merkle path"),
        )
        .expect("add sapling spend");
    builder
        .add_sapling_output::<core::convert::Infallible>(
            None,
            to,
            Zatoshis::const_from_u64(50_000),
            MemoBytes::empty(),
        )
        .expect("add sapling output");

    let build = builder
        .build(
            &TransparentSigningSet::new(),
            &[extsk],
            &[],
            rng,
            &tx_prover,
            &tx_prover,
            &ZeroFeeRule,
        )
        .expect("build tx");

    let tx = build.transaction();
    let mut tx_bytes = Vec::new();
    tx.write(&mut tx_bytes).expect("serialize tx");

    let tx_hex = bytes_to_hex(&tx_bytes);
    let branch_id = u32::from(zcash_protocol::consensus::BranchId::Sapling);

    let decoded = FluxTransaction::consensus_decode(&tx_bytes).expect("decode into flux tx");
    let shielded_params = load_params(&params_dir, FluxNetwork::Mainnet).expect("load params");
    verify_transaction(&decoded, branch_id, &shielded_params).expect("verify shielded proofs");

    println!("branch_id={branch_id}");
    println!("tx_hex={tx_hex}");
}
