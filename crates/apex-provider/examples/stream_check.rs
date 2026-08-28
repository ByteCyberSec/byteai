use apex_provider::{Client, StreamEvent};
use apex_types::Message;

#[tokio::main]
async fn main() {
    let key = std::env::var("HERMES_CUSTOM_API_B_AI_API_KEY").unwrap();
    let client = Client::new("https://api.b.ai/v1".to_string(), key).unwrap();
    let history = vec![Message::user("Reply with exactly: hello world")];
    let mut n = 0;
    client.chat_stream("deepseek-v4-flash", &history, &[], Some(100), |ev| {
        match ev {
            StreamEvent::Content(c) => { println!("[CONTENT] {c}"); n += 1; }
            StreamEvent::Reasoning(_) => { if n == 0 { print!("[R]"); } }
            StreamEvent::ToolCallDelta(i, id, name, args) => println!("[TOOL] {i} {id} {name} {args}"),
            StreamEvent::Usage(u) => println!("[USAGE] {}", u.total_tokens),
            StreamEvent::Done => println!("[DONE]"),
        }
    }).await.unwrap();
}
