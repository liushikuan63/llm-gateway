use base64::{engine::general_purpose::STANDARD as B64, Engine};
use llm_gateway_lib::crypto::{decrypt, encrypt, mask};
use std::sync::Once;

const TEST_MASTER_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

fn use_test_master_key() {
    static SET_TEST_MASTER_KEY: Once = Once::new();
    SET_TEST_MASTER_KEY.call_once(|| {
        // Integration tests run in a child process. This prevents crypto from reading or
        // creating the user's master.key while also exercising LLMGW_MASTER_KEY support.
        std::env::set_var("LLMGW_MASTER_KEY", TEST_MASTER_KEY);
    });
}

#[test]
fn encryption_round_trip() {
    use_test_master_key();

    let plaintext = "sk-test-\u{5bc6}\u{94a5}-123";
    let ciphertext = encrypt(plaintext).expect("encrypt test plaintext");

    assert_ne!(ciphertext, plaintext);
    assert_eq!(
        decrypt(&ciphertext).expect("decrypt test ciphertext"),
        plaintext
    );
}

#[test]
fn random_nonces_produce_distinct_ciphertexts() {
    use_test_master_key();

    let first = encrypt("same plaintext").expect("encrypt first plaintext");
    let second = encrypt("same plaintext").expect("encrypt second plaintext");

    assert_ne!(first, second);
    assert_eq!(
        decrypt(&first).expect("decrypt first ciphertext"),
        "same plaintext"
    );
    assert_eq!(
        decrypt(&second).expect("decrypt second ciphertext"),
        "same plaintext"
    );
}

#[test]
fn tampered_ciphertext_is_rejected() {
    use_test_master_key();

    let ciphertext = encrypt("integrity protected").expect("encrypt plaintext");
    let mut bytes = B64.decode(ciphertext).expect("decode ciphertext");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;

    assert!(decrypt(&B64.encode(bytes)).is_err());
}

#[test]
fn mask_short_secrets() {
    assert_eq!(mask("short"), "****");
    assert_eq!(mask("\u{5bc6}\u{94a5}\u{77ed}"), "****");
}

#[test]
fn mask_unicode_secrets_without_panicking() {
    assert_eq!(
        mask("\u{5929}\u{5730}\u{7384}\u{9ec4}\u{5b87}\u{5b99}\u{6d2a}\u{8352}\u{65e5}"),
        "\u{5929}\u{5730}\u{7384}\u{9ec4}****\u{5b99}\u{6d2a}\u{8352}\u{65e5}"
    );
}
