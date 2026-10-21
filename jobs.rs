//! Background jobs in a Postgres table (ADR 0002). A worker claims a due job
//! with `FOR UPDATE SKIP LOCKED` and holds it for a lease; if the worker
//! dies, the lease runs out and another worker picks the job up.

use std::sync::Arc;

use serde_json::Value;
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};

use super::clock::Clock;
use super::ids::{JobId, TenantId};

/// A job handed to a worker.
#[derive(Debug, Clone)]
pub struct ClaimedJob {
    /// Its ID.
    pub id: JobId,
    /// What to do, e.g. `"draft_reminders"`.
    pub kind: String,
    /// The job's input.
    pub payload: Value,
    /// Whose data it concerns, if anyone's.
    pub tenant: Option<TenantId>,
    /// Which attempt this is, starting at 1.
    pub attempts: i32,
}

/// Job queue errors.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// The job's lease ran out and another worker has claimed it since; this
    /// worker's result is discarded.
    #[error("lease lost: another worker has this job now")]
    LeaseLost,
    /// Database failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// The job queue.
#[derive(Clone)]
pub struct Jobs {
    pool: PgPool,
    clock: Arc<dyn Clock>,
}

impl Jobs {
    /// A queue on `pool`, reading time from `clock`.
    pub fn new(pool: PgPool, clock: Arc<dyn Clock>) -> Self {
        Self { pool, clock }
    }

    /// Adds a job to run at or after `run_at`.
    pub async fn enqueue(
        &self,
        kind: &str,
        payload: &Value,
        tenant: Option<TenantId>,
        run_at: OffsetDateTime,
    ) -> Result<JobId, JobError> {
        let id = JobId::new();
        sqlx::query!(
            "insert into jobs (id, tenant_id, kind, payload, status, run_at, created_at, updated_at)
             values ($1, $2, $3, $4, 'queued', $5, $6, $6)",
            id.as_uuid(),
            tenant.map(|t| t.as_uuid()),
            kind,
            payload,
            run_at,
            self.clock.now(),
        )
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Claims the next due job for `lease`, or `None` if nothing is due.
    pub async fn claim(&self, lease: Duration) -> Result<Option<ClaimedJob>, JobError> {
        let now = self.clock.now();
        let row = sqlx::query!(
            "update jobs
             set locked_until = $2, attempts = attempts + 1, updated_at = $1
             where id = (
                 select id from jobs
                 where status = 'queued' and run_at <= $1 and (locked_until is null or locked_until < $1)
                 order by run_at
                 limit 1
                 for update skip locked
             )
             returning id, kind, payload, tenant_id, attempts",
            now,
            now + lease,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| ClaimedJob {
            id: r.id.into(),
            kind: r.kind,
            payload: r.payload,
            tenant: r.tenant_id.map(TenantId::from),
            attempts: r.attempts,
        }))
    }

    /// Marks a claimed job done.
    pub async fn complete(&self, job: &ClaimedJob) -> Result<(), JobError> {
        let done = sqlx::query!(
            "update jobs set status = 'done', locked_until = null, updated_at = $3
             where id = $1 and attempts = $2 and status = 'queued'",
            job.id.as_uuid(),
            job.attempts,
            self.clock.now(),
        )
        .execute(&self.pool)
        .await?;
        fenced(done.rows_affected())
    }

    /// Records a failed attempt. With `retry_at` the job runs again then;
    /// without, it is given up on (`dead`) and left for a person.
    pub async fn fail(&self, job: &ClaimedJob, error: &str, retry_at: Option<OffsetDateTime>) -> Result<(), JobError> {
        let failed = sqlx::query!(
            "update jobs
             set status = case when $4::timestamptz is null then 'dead' else 'queued' end,
                 run_at = coalesce($4, run_at), locked_until = null, last_error = $3, updated_at = $5
             where id = $1 and attempts = $2 and status = 'queued'",
            job.id.as_uuid(),
            job.attempts,
            error,
            retry_at,
            self.clock.now(),
        )
        .execute(&self.pool)
        .await?;
        fenced(failed.rows_affected())
    }
}

/// `complete` and `fail` only touch the job if this worker's claim (its
/// attempt number) is still the latest one.
fn fenced(rows_affected: u64) -> Result<(), JobError> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(JobError::LeaseLost)
    }
}
