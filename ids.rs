//! Typed identifiers, so a tenant's ID can't be passed where a message's ID
//! belongs.

use uuid::Uuid;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type)]
        #[sqlx(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A new, time-ordered ID.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// The underlying UUID.
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(
    /// Whoever owns a set of engine data; in Kompleto, a Firm.
    TenantId
);
id_type!(
    /// One message in the outbox.
    MessageId
);
id_type!(
    /// One background job.
    JobId
);
