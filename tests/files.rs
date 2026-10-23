//! Seam 1 (M4): file encryption (F3). Every file has its own key, wrapped by
//! the master key and bound to the file and its tenant.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

use loopwright::files::{FileCipher, MasterKey};
use loopwright::ids::{FileId, TenantId};

fn cipher() -> FileCipher {
    FileCipher::new(&MasterKey::from_hex(&"ab".repeat(32)).unwrap())
}

#[test]
fn f3_a_file_round_trips_and_is_not_stored_in_the_clear() {
    let (tenant, id) = (TenantId::new(), FileId::new());
    let photo = b"JPEG of a sales invoice".to_vec();

    let sealed = cipher().seal(tenant, id, &photo).unwrap();

    assert!(!sealed.ciphertext.windows(photo.len()).any(|w| w == photo.as_slice()));
    assert_eq!(cipher().open(tenant, id, &sealed).unwrap(), photo);
}

#[test]
fn f3_a_changed_swapped_or_moved_file_cant_be_decrypted() {
    let (tenant, id) = (TenantId::new(), FileId::new());
    let sealed = cipher().seal(tenant, id, b"bank statement").unwrap();
    let other = cipher().seal(tenant, FileId::new(), b"other file").unwrap();

    let mut changed = sealed.clone();
    changed.ciphertext[30] ^= 1;
    let mut swapped_key = sealed.clone();
    swapped_key.wrapped_key = other.wrapped_key;
    let wrong_master = FileCipher::new(&MasterKey::from_hex(&"cd".repeat(32)).unwrap());

    assert!(cipher().open(tenant, id, &changed).is_err(), "a changed byte");
    assert!(cipher().open(tenant, id, &swapped_key).is_err(), "another file's key");
    assert!(cipher().open(TenantId::new(), id, &sealed).is_err(), "another tenant");
    assert!(
        cipher().open(tenant, FileId::new(), &sealed).is_err(),
        "another file ID"
    );
    assert!(wrong_master.open(tenant, id, &sealed).is_err(), "another master key");
}

#[test]
fn a_master_key_is_64_hex_digits_and_a_new_one_is_random() {
    assert!(MasterKey::from_hex("abc").is_err());
    assert!(MasterKey::from_hex(&"zz".repeat(32)).is_err());
    let (one, two) = (MasterKey::generate_hex().unwrap(), MasterKey::generate_hex().unwrap());
    assert_eq!(one.len(), 64);
    assert_ne!(one, two);
    MasterKey::from_hex(&one).unwrap();
}
