//! Opaque, sortable, globally unique identifiers (UUIDv7).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }
    };
}

typed_id!(TenantId);
typed_id!(FacilityId);
typed_id!(UserId);
typed_id!(PatientId);
typed_id!(EncounterId);
typed_id!(ServiceRequestId);
typed_id!(ObservationId);
typed_id!(DiagnosticReportId);
typed_id!(AlertId);
typed_id!(FollowUpTaskId);
typed_id!(AiArtifactId);
typed_id!(AuditEventId);
typed_id!(ConsentId);
typed_id!(EventId);
typed_id!(CorrelationId);
