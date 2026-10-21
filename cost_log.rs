//! The model cost log (J1): every call, with its model, tokens, cost and
//! the provider's request ID, so cost per Tenant per month is a query.

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;
use uuid::Uuid;

use super::ids::TenantId;
use super::llm::Usage;

/// List prices in billionths of a US dollar per token (input, output):
/// US$ per million tokens × 1,000. Source: `docs/research/llm-options.md`.
const PRICES: [(&str, u64, u64); 3] = [
    ("claude-haiku-5-5", 100, 500),
    ("claude-sonnet-5-5", 2_000, 10_000),
    ("claude-opus-5-5", 4_000, 20_000),
];

/// The cost of `usage` on `model`, or `None` for a model with no known price.
pub fn cost_nano_usd(model: &str, usage: Usage) -> Option<i64> {
    let (_, input, output) = PRICES.iter().find(|(name, _, _)| *name == model)?;
    let total = u64::from(usage.input_tokens) * input + u64::from(usage.output_tokens) * output;
    i64::try_from(total).ok()
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A usable answer.
    Ok,
    /// An answer the caller rejected.
    Invalid,
    /// No answer.
    Failed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Invalid => "invalid",
            Self::Failed => "failed",
        }
    }
}

/// One model call to record.
#[derive(Debug, Clone)]
pub struct LlmCall {
    /// Whose work it was.
    pub tenant: TenantId,
    /// When (from the Clock).
    pub at: OffsetDateTime,
    /// The model.
    pub model: String,
    /// Tokens used.
    pub usage: Usage,
    /// The provider's request ID, if it gave one.
    pub request_id: Option<String>,
    /// How it ended.
    pub outcome: Outcome,
}

/// Records one call.
pub async fn record(executor: impl PgExecutor<'_>, call: &LlmCall) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "insert into llm_calls (id, tenant_id, at, model, input_tokens, output_tokens, cost_nano_usd, request_id, outcome)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        Uuid::now_v7(),
        call.tenant.as_uuid(),
        call.at,
        call.model,
        i32::try_from(call.usage.input_tokens).unwrap_or(i32::MAX),
        i32::try_from(call.usage.output_tokens).unwrap_or(i32::MAX),
        cost_nano_usd(&call.model, call.usage),
        call.request_id,
        call.outcome.as_str(),
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Calls, tokens and cost over a time range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Number of calls.
    pub calls: i64,
    /// Input tokens.
    pub input_tokens: i64,
    /// Output tokens.
    pub output_tokens: i64,
    /// Cost of the calls whose model has a known price.
    pub cost_nano_usd: i64,
}

impl Totals {
    /// The cost in US dollars, for display.
    pub fn cost_usd(&self) -> f64 {
        // Display only; amounts stay integers everywhere else.
        #[expect(
            clippy::cast_precision_loss,
            reason = "a display value; cents-level precision is plenty"
        )]
        let dollars = self.cost_nano_usd as f64 / 1e9;
        dollars
    }
}

/// One Tenant's totals for calls made from `from` up to (not including) `to`.
pub async fn totals_for_tenant(
    pool: &PgPool,
    tenant: TenantId,
    from: OffsetDateTime,
    to: OffsetDateTime,
) -> Result<Totals, sqlx::Error> {
    let row = sqlx::query!(
        r#"select count(*) as "calls!", coalesce(sum(input_tokens), 0)::bigint as "input!",
                  coalesce(sum(output_tokens), 0)::bigint as "output!",
                  coalesce(sum(cost_nano_usd), 0)::bigint as "cost!"
           from llm_calls where tenant_id = $1 and at >= $2 and at < $3"#,
        tenant.as_uuid(),
        from,
        to,
    )
    .fetch_one(pool)
    .await?;
    Ok(Totals {
        calls: row.calls,
        input_tokens: row.input,
        output_tokens: row.output,
        cost_nano_usd: row.cost,
    })
}

/// Every Tenant's totals over a time range: the operator's report.
pub async fn totals_by_tenant(
    pool: &PgPool,
    from: OffsetDateTime,
    to: OffsetDateTime,
) -> Result<Vec<(TenantId, Totals)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"select tenant_id, count(*) as "calls!", coalesce(sum(input_tokens), 0)::bigint as "input!",
                  coalesce(sum(output_tokens), 0)::bigint as "output!",
                  coalesce(sum(cost_nano_usd), 0)::bigint as "cost!"
           from llm_calls where at >= $1 and at < $2
           group by tenant_id order by 5 desc"#,
        from,
        to,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let totals = Totals {
                calls: row.calls,
                input_tokens: row.input,
                output_tokens: row.output,
                cost_nano_usd: row.cost,
            };
            (row.tenant_id.into(), totals)
        })
        .collect())
}
