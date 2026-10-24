//! Seam 1 (M5): inbound email from sample messages (G1).

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

use nudgewright::inbound::{InboundError, MAX_RAW_BYTES, parse};

fn sample(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/mail/{name}.eml", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path)
        .unwrap()
        .replace("{{TO}}", "reply+abc@product.test")
        .into_bytes()
}

#[test]
fn g1_a_human_reply_is_read_with_its_sender_recipients_and_text() {
    let email = parse(&sample("human_reply")).unwrap();

    assert_eq!(email.from, "nena@example.test", "lowercased");
    assert_eq!(email.recipients, ["reply+abc@product.test"]);
    assert_eq!(email.subject, "Re: Mga dokumento para sa October 2026");
    assert!(email.text.starts_with("Hi po! Ipapadala ko po bukas"), "{}", email.text);
    assert!(!email.automatic);
    assert!(email.attachments.is_empty());
}

#[test]
fn g1_out_of_office_replies_are_recognised_as_automatic() {
    for name in ["outlook_ooo", "gmail_vacation", "subject_only_ooo"] {
        assert!(parse(&sample(name)).unwrap().automatic, "{name}");
    }
}

#[test]
fn g1_attachments_are_kept_as_bytes_for_the_upload_rules_to_judge() {
    let email = parse(&sample("with_attachments")).unwrap();

    assert_eq!(email.text.trim(), "Eto po ang resibo.");
    assert_eq!(email.attachments.len(), 2);
    assert!(email.attachments[0].starts_with(b"\xFF\xD8\xFF"));
    assert!(email.attachments[1].starts_with(b"MZ"));
}

#[test]
fn g1_unreadable_or_oversized_mail_is_refused() {
    assert!(matches!(parse(b"not an email at all"), Err(InboundError::Unreadable)));
    assert!(matches!(
        parse(&vec![b'a'; MAX_RAW_BYTES + 1]),
        Err(InboundError::TooLarge)
    ));
}

#[test]
fn g1_nul_characters_are_dropped_so_the_reply_can_be_stored() {
    let raw = b"From: nena@example.test\r\nTo: reply+abc@product.test\r\n\
Subject: =?utf-8?Q?Re:=00_docs?=\r\nContent-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\r\nHi=00 po\r\n";

    let email = parse(raw).unwrap();

    assert_eq!(email.subject, "Re: docs");
    assert_eq!(email.text.trim(), "Hi po");
}

#[test]
fn g1_a_small_signature_image_is_not_an_attachment_but_an_inline_photo_is() {
    let email = parse(&sample("signature_logo")).unwrap();

    assert_eq!(email.attachments.len(), 1);
    assert!(
        email.attachments[0].starts_with(b"\xFF\xD8\xFF"),
        "the photo, not the logo"
    );
}

#[test]
fn g1_the_email_worker_and_the_app_agree_on_the_signature() {
    // A product's email worker checks the same vector (Kompleto: `deploy/email-worker/worker.test.mjs`).
    let raw = b"From: nena@example.test\r\nSubject: Re: docs\r\n\r\nHi po\r\n";
    let vector = "62c376e06f8b7a0bddc963a7c17ee0bf0170a0aabeacbc8dd582574788e4e950";

    assert!(nudgewright::inbound::signed(b"test-inbound-secret", raw, vector));
}
