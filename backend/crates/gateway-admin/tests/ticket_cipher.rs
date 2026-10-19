//! fork：票据密文按账号隔离，搬动或篡改后拒绝解开。

use gateway_admin::ticket_cipher::TicketCipher;

#[test]
fn seals_per_account_and_rejects_moved_or_tampered_ciphertext() {
    let root = std::env::temp_dir().join(format!("cpr-ticket-{}", uuid::Uuid::now_v7()));
    let cipher = TicketCipher::new(root.clone());
    let sealed = cipher.seal("acct_a", b"secret").unwrap();
    assert_eq!(cipher.open("acct_a", &sealed).unwrap(), b"secret");
    assert!(cipher.open("acct_b", &sealed).is_err());
    let mut tampered = sealed.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(cipher.open("acct_a", &tampered).is_err());
    // 同一明文两次加密使用不同 nonce。
    assert_ne!(cipher.seal("acct_a", b"secret").unwrap(), sealed);
    std::fs::remove_dir_all(root).unwrap();
}
