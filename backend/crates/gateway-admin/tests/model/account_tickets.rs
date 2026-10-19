//! fork：票据行解析、TOTP 密钥归一化、邮箱脱敏与金额归一化。

use gateway_admin::model::account_tickets::{TicketSecret, mask_email, normalize_amount};
use secrecy::ExposeSecret as _;

#[test]
fn parses_ticket_line_and_normalizes_totp_secret() {
    let ticket = TicketSecret::parse_line(" a.b@example.com----p@ss----abcd efgh 2345 ").unwrap();
    assert_eq!(ticket.email, "a.b@example.com");
    assert_eq!(ticket.password.expose_secret(), "p@ss");
    assert_eq!(ticket.totp_secret.expose_secret(), "ABCDEFGH2345");
    let restored = TicketSecret::from_plaintext(&ticket.to_plaintext()).unwrap();
    assert_eq!(restored.email, ticket.email);
    assert_eq!(restored.totp_secret.expose_secret(), "ABCDEFGH2345");
}

#[test]
fn rejects_malformed_ticket_lines() {
    assert!(TicketSecret::parse_line("no-at----pw----ABC").is_err());
    assert!(TicketSecret::parse_line("a@b.c----").is_err());
    assert!(TicketSecret::parse_line("a@b.c----pw----not-base32!").is_err());
    assert!(TicketSecret::parse_line("a@b.c----pw").is_ok());
}

#[test]
fn masks_email_and_normalizes_amount() {
    assert_eq!(mask_email("dennyfuo@gmail.com"), "d***o@gmail.com");
    assert_eq!(mask_email("a@x.io"), "a***@x.io");
    assert_eq!(normalize_amount("55").unwrap(), "55.00");
    assert_eq!(normalize_amount("55.5").unwrap(), "55.50");
    assert!(normalize_amount("-1").is_err());
    assert!(normalize_amount("1.234").is_err());
}
