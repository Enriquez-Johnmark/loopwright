//! Seam 6 (M3): the AI cost log (J1).

#![allow(clippy::unwrap_used, reason = "test code: a failed unwrap is a failed test")]

use nudgewright::cost_log::{self, LlmCall, Outcome};
use nudgewright::ids::TenantId;
use nudgewright::llm::Usage;
use sqlx::PgPool;
use time::macros::datetime;

fn call(tenant: TenantId, at: time::OffsetDateTime, input: u32, output: u32) -> LlmCall {
    LlmCall {
        tenant,
        at,
        model: "claude-haiku-5-5".to_owned(),
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
        },
        request_id: Some("req_1".to_owned()),
        outcome: Outcome::Ok,
    }
}

#[test]
fn j1_cost_follows_the_models_price_per_token() {
    let usage = Usage {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
    };
    // Haiku 5.5: $0.10 in + $0.50 out per million tokens.
    assert_eq!(cost_log::cost_nano_usd("claude-haiku-5-5", usage), Some(600_000_000));
    assert_eq!(
        cost_log::cost_nano_usd("claude-sonnet-5-5", usage),
        Some(12_000_000_000)
    );
    assert_eq!(
        cost_log::cost_nano_usd("claude-haiku-5-5-20261001", usage),
        Some(600_000_000),
        "a dated snapshot of the same model"
    );
    assert_eq!(cost_log::cost_nano_usd("some-new-model", usage), None);
}

#[sqlx::test]
async fn j1_every_ai_call_is_recorded_and_summed_per_firm_per_month(pool: PgPool) {
    let (santos, reyes) = (TenantId::new(), TenantId::new());
    for record in [
        call(santos, datetime!(2026-10-31 15:59 UTC), 1_000, 100), // 31 Oct, 23:59 Manila
        call(santos, datetime!(2026-10-31 16:00 UTC), 2_000, 200), // 1 Nov, 00:00 Manila
        call(santos, datetime!(2026-11-20 03:00 UTC), 3_000, 300),
        call(reyes, datetime!(2026-11-02 03:00 UTC), 500, 50),
    ] {
        cost_log::record(&pool, &record).await.unwrap();
    }

    // November in Manila time.
    let (from, to) = (datetime!(2026-11-01 0:00 +8), datetime!(2026-12-01 0:00 +8));
    let november = cost_log::totals_for_tenant(&pool, santos, from, to).await.unwrap();
    assert_eq!(
        (november.calls, november.input_tokens, november.output_tokens),
        (2, 5_000, 500)
    );
    assert_eq!(november.cost_nano_usd, 5_000 * 100 + 500 * 500);

    let report = cost_log::totals_by_tenant(&pool, from, to).await.unwrap();
    assert_eq!(report.len(), 2);
    assert!(
        report
            .iter()
            .any(|(tenant, month)| *tenant == reyes && month.calls == 1)
    );
}
