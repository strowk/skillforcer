use crate::model::Decision;
use serde_json::json;

pub fn render_decision(d: &Decision) -> Option<serde_json::Value> {
    match d {
        Decision::Allow => None,
        Decision::Deny { reason } => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Decision;

    #[test]
    fn allow_renders_nothing() {
        assert_eq!(render_decision(&Decision::Allow), None);
    }

    #[test]
    fn deny_renders_permission_json() {
        let v = render_decision(&Decision::Deny { reason: "load it".into() }).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "load it");
    }
}
