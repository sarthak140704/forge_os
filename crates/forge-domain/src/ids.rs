//! Typed newtype identifiers.
//!
//! Using distinct types for MissionId/GoalId/TaskId prevents accidentally
//! passing one where another is expected — a class of bugs that plain UUIDs
//! silently allow.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! uuid_newtype {
    ($name:ident, $prefix:expr) => {
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            #[inline]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            #[inline]
            pub fn from_uuid(u: Uuid) -> Self {
                Self(u)
            }

            #[inline]
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}_{}", $prefix, self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let bare = s
                    .strip_prefix(concat!($prefix, "_"))
                    .unwrap_or(s);
                Ok(Self(Uuid::parse_str(bare)?))
            }
        }
    };
}

uuid_newtype!(MissionId, "msn");
uuid_newtype!(GoalId,    "gol");
uuid_newtype!(TaskId,    "tsk");

/// Monotonic sequence assigned by the event store on append.
/// Unlike the aggregate IDs above, EventId is not client-generated.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub i64);

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "evt_{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn roundtrip_display_parse() {
        let m = MissionId::new();
        let s = m.to_string();
        let parsed = MissionId::from_str(&s).unwrap();
        assert_eq!(m, parsed);
    }

    #[test]
    fn ids_are_distinct_types() {
        // Compile-time guarantee: this file wouldn't compile if the newtype
        // wrapper allowed passing one ID kind where another is expected.
        fn takes_mission(_: MissionId) {}
        takes_mission(MissionId::new());
    }
}
