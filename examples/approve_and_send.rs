//! The whole loop in one program: draft a message, approve exactly what was
//! seen, and send it once. Run it against the docker-compose Postgres and a
//! local Mailpit (http://localhost:8025):
//!
//! ```sh
//! cargo run --example approve_and_send
//! ```

use std::sync::Arc;

use loopwright::clock::{Clock, SystemClock};
use loopwright::ids::TenantId;
use loopwright::mail::{Email, MailpitMailer};
use loopwright::outbox::{NoExtraChecks, Outbox, Sender};
use sqlx::PgPool;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL")?;
    let pool = PgPool::connect(&url).await?;
    loopwright::MIGRATOR.run(&pool).await?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let outbox = Outbox::new(pool.clone(), clock.clone());

    // 1. A product drafts a message for one of its tenants.
    let tenant = TenantId::new();
    let email = Email {
        to_address: "client@example.test".to_owned(),
        from_name: "Example Firm".to_owned(),
        reply_to: None,
        subject: "Documents for September".to_owned(),
        body_text: "Hi! Please send your September receipts by Friday.".to_owned(),
    };
    let id = outbox.create_draft(tenant, &email).await?;

    // 2. A person approves exactly the content they were shown.
    let seen = outbox.get(tenant, id).await?.content_hash();
    outbox
        .approve(tenant, id, "owner@example.test", &seen, &NoExtraChecks)
        .await?;

    // 3. The sender delivers it once, even if this program crashes and runs again.
    let mailer = MailpitMailer::new("http://localhost:8025", "reminders@example.test")?;
    let sender = Sender::new(pool, mailer, clock, Arc::new(NoExtraChecks));
    let report = sender.drain_once(10).await?;
    println!("{report:?}");
    Ok(())
}
