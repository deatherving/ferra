use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ChangeEvent {
    pub event_id: i64,
    pub key: String,
    pub operation: Operation,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Set,
    Delete,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Delete => "delete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChangeEvent, Operation};

    #[test]
    fn operation_as_str() {
        assert_eq!(Operation::Set.as_str(), "set");
        assert_eq!(Operation::Delete.as_str(), "delete");
    }

    #[test]
    fn operation_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Operation::Set).unwrap(), "\"set\"");
        assert_eq!(
            serde_json::to_string(&Operation::Delete).unwrap(),
            "\"delete\""
        );
    }

    #[test]
    fn change_event_serializes() {
        let ev = ChangeEvent {
            event_id: 7,
            key: "k".into(),
            operation: Operation::Set,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"event_id\":7"));
        assert!(s.contains("\"key\":\"k\""));
        assert!(s.contains("\"operation\":\"set\""));
    }
}
