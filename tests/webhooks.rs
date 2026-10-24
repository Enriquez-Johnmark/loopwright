//! Seam 2 (M5): the provider's signed delivery events (G3).

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use nudgewright::webhooks::{DeliveryEvent, Signature, parse, verified};
use sha2::Sha256;
use time::macros::datetime;

const SECRET: &str = "whsec_c2VjcmV0LWtleS1mb3ItdGVzdHM=";

fn sign(id: &str, timestamp: &str, body: &[u8]) -> String {
    let key = STANDARD.decode(SECRET.trim_start_matches("whsec_")).unwrap();
    let mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key).unwrap();
    let digest = mac
        .chain_update(format!("{id}.{timestamp}.").as_bytes())
        .chain_update(body)
        .finalize()
        .into_bytes();
    format!("v1,{}", STANDARD.encode(digest))
}

const BOUNCE: &[u8] = br#"{"type":"email.bounced","created_at":"2026-11-01T00:00:00Z","data":{"email_id":"re_123","to":["nena@example.test"],"bounce":{"type":"Permanent","subType":"NoEmail","message":"Mailbox does not exist"}}}"#;

#[test]
fn g3_a_signed_recent_delivery_is_verified() {
    let now = datetime!(2026-11-01 00:00:30 UTC);
    let timestamp = now.unix_timestamp().to_string();
    let signature = sign("msg_1", &timestamp, BOUNCE);

    let given = Signature {
        id: "msg_1",
        timestamp: &timestamp,
        signatures: &format!("v1,bm90LXRoaXMtb25l {signature}"),
    };

    assert!(verified(SECRET, given, BOUNCE, now));
}

#[test]
fn g3_a_forged_altered_or_replayed_delivery_is_refused() {
    let now = datetime!(2026-11-01 00:00:30 UTC);
    let timestamp = now.unix_timestamp().to_string();
    let good = sign("msg_1", &timestamp, BOUNCE);
    let old = (now.unix_timestamp() - 6 * 60).to_string();
    let replayed = sign("msg_1", &old, BOUNCE);
    let altered = BOUNCE.to_vec().into_iter().rev().collect::<Vec<_>>();

    for (case, timestamp, signatures, body) in [
        ("no signature", timestamp.as_str(), "", BOUNCE),
        ("another body", timestamp.as_str(), good.as_str(), altered.as_slice()),
        ("over 5 minutes old", old.as_str(), replayed.as_str(), BOUNCE),
        ("not a v1 signature", timestamp.as_str(), "v2,abc", BOUNCE),
        ("an absurd timestamp", "-9223372036854775808", good.as_str(), BOUNCE),
    ] {
        let given = Signature {
            id: "msg_1",
            timestamp,
            signatures,
        };
        assert!(!verified(SECRET, given, body, now), "{case}");
    }
    let wrong_secret = "whsec_b3RoZXI=";
    let given = Signature {
        id: "msg_1",
        timestamp: &timestamp,
        signatures: &good,
    };
    assert!(!verified(wrong_secret, given, BOUNCE, now), "another secret");
}

#[test]
fn g3_bounces_and_complaints_are_read_from_the_event() {
    let transient = String::from_utf8(BOUNCE.to_vec())
        .unwrap()
        .replace("Permanent", "Transient");
    let complaint = br#"{"type":"email.complained","data":{"email_id":"re_9"}}"#;
    let delivered = br#"{"type":"email.delivered","data":{"email_id":"re_9"}}"#;

    assert_eq!(
        parse(BOUNCE),
        Some(DeliveryEvent::Bounced {
            provider_message_id: "re_123".to_owned(),
            permanent: true,
            reason: "Mailbox does not exist".to_owned(),
        })
    );
    let undetermined = transient.replace("Transient", "Undetermined");
    for maybe in [transient, undetermined] {
        assert!(matches!(
            parse(maybe.as_bytes()),
            Some(DeliveryEvent::Bounced { permanent: false, .. })
        ));
    }
    assert_eq!(
        parse(complaint),
        Some(DeliveryEvent::Complained {
            provider_message_id: "re_9".to_owned()
        })
    );
    assert_eq!(parse(delivered), Some(DeliveryEvent::Other));
    assert_eq!(parse(b"not json"), None);
}

#[test]
fn g3_events_about_domains_or_contacts_are_taken_and_ignored() {
    let domain =
        br#"{"type":"domain.updated","created_at":"2026-11-02T00:00:00Z","data":{"id":"d_1","name":"product.example"}}"#;
    let contact = br#"{"type":"contact.created","data":{"id":"c_1","email":"x@example.test"}}"#;
    let idless_bounce = br#"{"type":"email.bounced","data":{"bounce":{"type":"Permanent"}}}"#;

    assert_eq!(parse(domain), Some(DeliveryEvent::Other));
    assert_eq!(parse(contact), Some(DeliveryEvent::Other));
    assert_eq!(parse(idless_bounce), None, "a bounce must name its email");
}
