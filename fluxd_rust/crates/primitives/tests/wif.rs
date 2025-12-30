use fluxd_consensus::Network;
use fluxd_primitives::{secret_key_to_wif, wif_to_secret_key, AddressError};

#[test]
fn wif_roundtrips_mainnet() {
    let secret = [0x11u8; 32];

    let wif_uncompressed = secret_key_to_wif(&secret, Network::Mainnet, false);
    let (decoded, compressed) =
        wif_to_secret_key(&wif_uncompressed, Network::Mainnet).expect("decode mainnet wif");
    assert_eq!(decoded, secret);
    assert!(!compressed);

    let wif_compressed = secret_key_to_wif(&secret, Network::Mainnet, true);
    let (decoded, compressed) =
        wif_to_secret_key(&wif_compressed, Network::Mainnet).expect("decode mainnet wif");
    assert_eq!(decoded, secret);
    assert!(compressed);
}

#[test]
fn wif_roundtrips_testnet() {
    let secret = [0x22u8; 32];
    let wif = secret_key_to_wif(&secret, Network::Testnet, true);
    let (decoded, compressed) = wif_to_secret_key(&wif, Network::Testnet).expect("decode");
    assert_eq!(decoded, secret);
    assert!(compressed);
}

#[test]
fn wif_rejects_wrong_network() {
    let secret = [0x33u8; 32];
    let wif = secret_key_to_wif(&secret, Network::Mainnet, false);
    let err = wif_to_secret_key(&wif, Network::Testnet).unwrap_err();
    assert!(matches!(err, AddressError::UnknownPrefix));
}
