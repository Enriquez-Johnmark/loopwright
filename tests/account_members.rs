//! An account's members: one owner and up to five staff.

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use common::clock;
use nudgewright::accounts::{Accounts, MAX_STAFF, MemberError, Members, Role};
use nudgewright::ids::TenantId;
use sqlx::PgPool;

async fn account(pool: &PgPool) -> TenantId {
    Accounts::new(pool.clone(), clock())
        .create("Reyes Creative", "ana@reyes.test")
        .await
        .unwrap()
}

#[sqlx::test]
async fn list_shows_the_owner_first_then_staff(pool: PgPool) {
    let tenant = account(&pool).await;
    let members = Members::new(pool, clock());
    members
        .add_staff(tenant, "ben@reyes.test", "ana@reyes.test")
        .await
        .unwrap();

    let list = members.list(tenant).await.unwrap();

    let summary: Vec<_> = list.iter().map(|m| (m.email.as_str(), m.role)).collect();
    assert_eq!(
        summary,
        [("ana@reyes.test", Role::Owner), ("ben@reyes.test", Role::Staff)]
    );
}

#[sqlx::test]
async fn add_staff_stops_at_the_limit(pool: PgPool) {
    let tenant = account(&pool).await;
    let members = Members::new(pool, clock());
    for n in 0..MAX_STAFF {
        members
            .add_staff(tenant, &format!("staff{n}@reyes.test"), "ana@reyes.test")
            .await
            .unwrap();
    }

    let err = members
        .add_staff(tenant, "one-more@reyes.test", "ana@reyes.test")
        .await
        .unwrap_err();

    assert!(matches!(err, MemberError::TooManyStaff), "got {err:?}");
}

#[sqlx::test]
async fn add_staff_refuses_an_address_that_logs_in_elsewhere(pool: PgPool) {
    let tenant = account(&pool).await;
    Accounts::new(pool.clone(), clock())
        .create("Other Studio", "cara@other.test")
        .await
        .unwrap();
    let members = Members::new(pool, clock());

    let err = members
        .add_staff(tenant, "Cara@other.test", "ana@reyes.test")
        .await
        .unwrap_err();

    assert!(matches!(err, MemberError::EmailTaken), "got {err:?}");
}

#[sqlx::test]
async fn remove_staff_refuses_the_owner(pool: PgPool) {
    let tenant = account(&pool).await;
    let members = Members::new(pool, clock());
    let owner = members.list(tenant).await.unwrap()[0].id;

    let err = members.remove_staff(tenant, owner, "ana@reyes.test").await.unwrap_err();

    assert!(matches!(err, MemberError::IsOwner), "got {err:?}");
}

#[sqlx::test]
async fn remove_staff_takes_them_off_the_list(pool: PgPool) {
    let tenant = account(&pool).await;
    let members = Members::new(pool, clock());
    let ben = members
        .add_staff(tenant, "ben@reyes.test", "ana@reyes.test")
        .await
        .unwrap();

    members.remove_staff(tenant, ben.id, "ana@reyes.test").await.unwrap();

    assert_eq!(members.list(tenant).await.unwrap().len(), 1);
}

#[sqlx::test]
async fn another_tenant_cannot_remove_a_member(pool: PgPool) {
    let tenant = account(&pool).await;
    let other = Accounts::new(pool.clone(), clock())
        .create("Other Studio", "cara@other.test")
        .await
        .unwrap();
    let members = Members::new(pool, clock());
    let ben = members
        .add_staff(tenant, "ben@reyes.test", "ana@reyes.test")
        .await
        .unwrap();

    let err = members
        .remove_staff(other, ben.id, "cara@other.test")
        .await
        .unwrap_err();

    assert!(matches!(err, MemberError::NotFound), "got {err:?}");
}

#[sqlx::test]
async fn adding_staff_is_recorded_in_the_audit_log(pool: PgPool) {
    let tenant = account(&pool).await;
    let members = Members::new(pool.clone(), clock());

    members
        .add_staff(tenant, "ben@reyes.test", "ana@reyes.test")
        .await
        .unwrap();

    let actor = sqlx::query_scalar::<_, String>(
        "select actor from audit_events where tenant_id = $1 and action = 'account.member_added'",
    )
    .bind(tenant.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(actor, "ana@reyes.test");
}
