//! Seam 4: the Postgres job queue (enqueue → claim → finish).

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

mod common;

use std::collections::HashSet;

use common::clock;
use loopwright::jobs::{JobError, Jobs};
use serde_json::json;
use sqlx::PgPool;
use time::Duration;

const LEASE: Duration = Duration::minutes(5);

#[sqlx::test]
async fn a_job_is_not_claimed_before_its_run_at(pool: PgPool) {
    let clock = clock();
    let jobs = Jobs::new(pool, clock.clone());
    jobs.enqueue("draft_reminders", &json!({}), None, clock.now_plus(Duration::hours(1)))
        .await
        .unwrap();

    let claimed = jobs.claim(LEASE).await.unwrap();

    assert!(claimed.is_none(), "claimed a job before it was due: {claimed:?}");
}

#[sqlx::test]
async fn a_leased_job_is_not_claimed_twice(pool: PgPool) {
    let clock = clock();
    let jobs = Jobs::new(pool, clock.clone());
    jobs.enqueue("draft_reminders", &json!({}), None, clock.now_plus(Duration::ZERO))
        .await
        .unwrap();
    jobs.claim(LEASE).await.unwrap();

    let second = jobs.claim(LEASE).await.unwrap();

    assert!(second.is_none(), "a leased job was handed out again: {second:?}");
}

#[sqlx::test]
async fn workers_claiming_at_the_same_time_never_get_the_same_job(pool: PgPool) {
    let clock = clock();
    let jobs = Jobs::new(pool, clock.clone());
    for n in 0..5 {
        jobs.enqueue(
            "draft_reminders",
            &json!({ "n": n }),
            None,
            clock.now_plus(Duration::ZERO),
        )
        .await
        .unwrap();
    }

    let workers: Vec<_> = (0..10)
        .map(|_| {
            let jobs = jobs.clone();
            tokio::spawn(async move { jobs.claim(LEASE).await.unwrap() })
        })
        .collect();
    let mut claimed = Vec::new();
    for worker in workers {
        claimed.extend(worker.await.unwrap().map(|job| job.id));
    }

    let unique: HashSet<_> = claimed.iter().collect();
    assert!(
        claimed.len() == 5 && unique.len() == 5,
        "expected 5 distinct claims, got {claimed:?}"
    );
}

#[sqlx::test]
async fn a_job_whose_worker_died_is_claimed_again_after_the_lease(pool: PgPool) {
    let clock = clock();
    let jobs = Jobs::new(pool, clock.clone());
    jobs.enqueue("draft_reminders", &json!({}), None, clock.now_plus(Duration::ZERO))
        .await
        .unwrap();
    jobs.claim(LEASE).await.unwrap();
    clock.advance(LEASE + Duration::seconds(1));

    let again = jobs.claim(LEASE).await.unwrap();

    assert!(again.is_some(), "an abandoned job was never picked up again");
}

#[sqlx::test]
async fn a_completed_job_is_never_claimed_again(pool: PgPool) {
    let clock = clock();
    let jobs = Jobs::new(pool, clock.clone());
    jobs.enqueue("draft_reminders", &json!({}), None, clock.now_plus(Duration::ZERO))
        .await
        .unwrap();
    let job = jobs.claim(LEASE).await.unwrap().unwrap();
    jobs.complete(&job).await.unwrap();
    clock.advance(LEASE + Duration::seconds(1));

    let again = jobs.claim(LEASE).await.unwrap();

    assert!(again.is_none(), "a finished job ran again: {again:?}");
}

#[sqlx::test]
async fn a_failed_job_retries_at_the_given_time(pool: PgPool) {
    let clock = clock();
    let jobs = Jobs::new(pool, clock.clone());
    jobs.enqueue("draft_reminders", &json!({}), None, clock.now_plus(Duration::ZERO))
        .await
        .unwrap();
    let job = jobs.claim(LEASE).await.unwrap().unwrap();
    jobs.fail(&job, "LLM timed out", Some(clock.now_plus(Duration::minutes(10))))
        .await
        .unwrap();
    clock.advance(Duration::minutes(10));

    let retried = jobs.claim(LEASE).await.unwrap();

    assert!(
        retried.is_some_and(|j| j.attempts == 2),
        "the failed job wasn't retried as attempt 2"
    );
}

#[sqlx::test]
async fn a_worker_whose_lease_ran_out_cannot_finish_a_job_another_worker_took(pool: PgPool) {
    let clock = clock();
    let jobs = Jobs::new(pool, clock.clone());
    jobs.enqueue("draft_reminders", &json!({}), None, clock.now_plus(Duration::ZERO))
        .await
        .unwrap();
    let slow = jobs.claim(LEASE).await.unwrap().unwrap();
    clock.advance(LEASE + Duration::seconds(1));
    jobs.claim(LEASE).await.unwrap().unwrap();

    let result = jobs.complete(&slow).await;

    assert!(matches!(result, Err(JobError::LeaseLost)), "got {result:?}");
}
