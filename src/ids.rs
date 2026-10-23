//! Typed identifiers, so a tenant's ID can't be passed where a message's ID
//! belongs.

/// Defines a typed ID: a `Uuid` newtype that is time-ordered, serializable
/// and stored as a `uuid` column. Products use it for their own IDs too; the
/// crate using it needs `uuid`, `serde` and `sqlx` as dependencies.
#[macro_export]
macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize, ::sqlx::Type)]
        #[sqlx(transparent)]
        pub struct $name(::uuid::Uuid);

        impl $name {
            /// A new, time-ordered ID.
            pub fn new() -> Self {
                Self(::uuid::Uuid::now_v7())
            }

            /// The underlying UUID.
            pub fn as_uuid(&self) -> ::uuid::Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<::uuid::Uuid> for $name {
            fn from(id: ::uuid::Uuid) -> Self {
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

pub use crate::id_type;

id_type!(
    /// Whoever owns a set of engine data, e.g. a customer business.
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
id_type!(
    /// One stored, encrypted file.
    FileId
);
