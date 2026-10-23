//! Erasing everything the engine holds for one tenant, when its account
//! closes: messages, jobs, model-call costs, stored files and the audit
//! log. Safe to repeat after a crash part way.

use sqlx::PgPool;

use super::files::{FileError, Files};
use super::ids::TenantId;

/// Deletes `tenant`'s files (from the store, then their keys) and every
/// engine row of `tenant`.
pub async fn erase(pool: &PgPool, files: &Files, tenant: TenantId) -> Result<(), FileError> {
    files.erase_tenant(tenant).await?;
    let mut tx = pool.begin().await?;
    for sql in [
        "delete from outbox_messages where tenant_id = $1",
        "delete from jobs where tenant_id = $1",
        "delete from llm_calls where tenant_id = $1",
    ] {
        sqlx::query(sql).bind(tenant.as_uuid()).execute(&mut *tx).await?;
    }
    // The only way past the audit log's append-only trigger: it lets this
    // transaction delete this tenant's entries, and no others.
    sqlx::query("select set_config('engine.erasing_tenant', $1, true)")
        .bind(tenant.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query!("delete from audit_events where tenant_id = $1", tenant.as_uuid())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
