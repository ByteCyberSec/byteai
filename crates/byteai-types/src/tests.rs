#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use crate::{Message, ToolCall};

    #[test]
    fn wire_shape_for_tool_round() {
        let history = [
            Message::system("sys"),
            Message::user("u"),
            Message::assistant(
                Some("calling".to_string()),
                Some(vec![ToolCall { id: "call_1".into(), name: "shell".into(), arguments: "{\"command\":\"ls\"}".into() }]),
                None,
            ),
            Message::tool("call_1", "shell", "ok"),
        ];
        for m in &history {
            let _ = serde_json::to_string(&m.to_wire()).unwrap();
        }
        let tool_msg = history[3].to_wire();
        assert!(tool_msg.get("tool_call_id").is_some(), "tool message must carry tool_call_id");
        let asst = history[2].to_wire();
        assert!(asst.get("tool_calls").is_some());
    }

    #[test]
    fn empty_call_id_still_present() {
        let m = Message::tool("", "shell", "out");
        let wire = serde_json::to_string(&m.to_wire()).unwrap();
        assert!(wire.contains("tool_call_id"));
    }

    #[test]
    fn answer_is_forty_two() {
        fn answer() -> i32 { 42 }
        assert_eq!(answer(), 42);
    }
}
