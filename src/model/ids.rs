use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn parse(value: &str) -> Result<Self, uuid::Error> {
                Uuid::parse_str(value).map(Self)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                Display::fmt(&self.0, formatter)
            }
        }
    };
}

id_type!(SessionId);
id_type!(RunId);
id_type!(OperationId);

#[cfg(test)]
mod tests {
    use super::{OperationId, RunId, SessionId};

    #[test]
    fn typed_ids_round_trip_without_becoming_interchangeable_strings() {
        let session = SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap();
        let run = RunId::parse("22222222-2222-4222-8222-222222222222").unwrap();
        let operation = OperationId::parse("33333333-3333-4333-8333-333333333333").unwrap();

        assert_eq!(session.to_string(), "11111111-1111-4111-8111-111111111111");
        assert_eq!(run.to_string(), "22222222-2222-4222-8222-222222222222");
        assert_eq!(
            operation.to_string(),
            "33333333-3333-4333-8333-333333333333"
        );
        assert!(SessionId::parse("not-a-uuid").is_err());
    }
}
