//! `kanban` — lightweight kanban board persisted to disk.
//!
//! Boards live under <data_dir>/kanban/<board>.json. Cards can carry an
//! optional `task` payload the agent can hand to a worker. Actions:
//!   * boards                       — list boards
//!   * board <name>                 — create/reset a board
//!   * columns <name> [cols...]     — list columns or (re)set them (default To Do / In Progress / Done)
//!   * add <board> <col> <title> [body] [id]
//!   * move <board> <id> <to-col>
//!   * done <board> <id>
//!   * show <board> [col]           — print cards per column (compact)
//!   * delete <board> <id>          — remove a card

use std::path::PathBuf;
use std::time::Instant;

use apex_types::{ToolDef, ToolOutcome};
use serde_json::{Value, json};
use tokio::fs;

use crate::{BoxFuture, Tool, ok_outcome};

#[derive(Clone)]
pub struct KanbanTool {
    data_dir: PathBuf,
}

impl KanbanTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
    fn boards_dir(&self) -> PathBuf {
        self.data_dir.join("kanban")
    }
    fn board_path(&self, board: &str) -> PathBuf {
        self.boards_dir().join(format!("{board}.json"))
    }
}

impl Tool for KanbanTool {
    fn name(&self) -> &'static str {
        "kanban"
    }
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "kanban".into(),
            description: "Persistent kanban board: create boards, add/move/complete cards across columns. Input: {action: boards|board|columns|add|move|done|show|delete, board, col?, title?, body?, id?, columns?}.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["boards", "board", "columns", "add", "move", "done", "show", "delete"]},
                    "board": {"type": "string"},
                    "col": {"type": "string"},
                    "columns": {"type": "array", "items": {"type": "string"}},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "id": {"type": "string"}
                },
                "required": ["action"]
            }),
        }
    }

    fn execute(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        let self2 = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            let action = args.get("action").and_then(Value::as_str).unwrap_or("boards").to_string();
            let board = args.get("board").and_then(Value::as_str).unwrap_or("").to_string();
            let col = args.get("col").and_then(Value::as_str).unwrap_or("").to_string();
            let title = args.get("title").and_then(Value::as_str).unwrap_or("").to_string();
            let body = args.get("body").and_then(Value::as_str).unwrap_or("").to_string();
            let id = args.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let columns: Vec<String> = args.get("columns")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();

            fs::create_dir_all(self2.boards_dir()).await.ok();

            let out = match action.as_str() {
                "boards" => {
                    let mut names = Vec::new();
                    if let Ok(mut rd) = fs::read_dir(self2.boards_dir()).await {
                        while let Ok(Some(e)) = rd.next_entry().await {
                            if let Some(n) = e.file_name().to_str()
                                && n.ends_with(".json") {
                                    names.push(n.trim_end_matches(".json").to_string());
                                }
                        }
                    }
                    names.sort();
                    if names.is_empty() {
                        "no boards yet — `kanban board <name>` to create one".to_string()
                    } else {
                        format!("boards:\n  {}", names.join("\n  "))
                    }
                }
                "board" | "columns" => {
                    if board.is_empty() {
                        format!("usage: kanban {action} <board> [columns...]")
                    } else {
                        let path = self2.board_path(&board);
                        let data = match fs::read_to_string(&path).await {
                            Ok(s) => json_parse(&s).unwrap_or_else(|_| default_board()),
                            Err(_) => default_board(),
                        };
                        let mut b = data;
                        if action == "columns" && !columns.is_empty() {
                            b["columns"] = json!(columns);
                        }
                        // Drop card columns no longer in the list.
                        if let Some(cols) = b["columns"].as_array() {
                            let valid: Vec<String> = cols.iter().filter_map(|c| c.as_str().map(String::from)).collect();
                            if let Some(cards) = b["cards"].as_object_mut() {
                                cards.retain(|k, _| valid.contains(k));
                            }
                        }
                        if let Err(e) = fs::write(&path, serde_json::to_string_pretty(&b).unwrap_or_default()).await {
                            format!("write failed: {e:#}")
                        } else {
                            let cols = b["columns"].as_array().cloned().unwrap_or_default();
                            format!("board `{board}` columns: {}", cols.iter().filter_map(|c| c.as_str()).collect::<Vec<_>>().join(" → "))
                        }
                    }
                }
                "add" => {
                    if board.is_empty() || col.is_empty() || title.is_empty() {
                        "usage: kanban add <board> <col> <title> [body] [id]".to_string()
                    } else {
                        let path = self2.board_path(&board);
                        let mut b = load_board(&path).await;
                        let cols: Vec<String> = b["columns"].as_array()
                            .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
                            .unwrap_or_default();
                        let col_name = if cols.iter().any(|c| c == &col) { col.clone() } else {
                            if !cols.is_empty() { cols[0].clone() } else { "To Do".to_string() }
                        };
                        let card_id = if id.is_empty() {
                            format!("c{}", (b["seq"].as_u64().unwrap_or(0) + 1))
                        } else {
                            id.clone()
                        };
                        b["seq"] = json!(b["seq"].as_u64().unwrap_or(0) + 1);
                        let card = json!({ "title": title, "body": body, "created": now_iso() });
                        if !b["cards"].is_object() { b["cards"] = json!({}); }
                        b["cards"][&col_name][&card_id] = card;
                        write_board(&path, &b).await;
                        format!("added card `{card_id}` to `{board}` / {col_name}: {title}")
                    }
                }
                "move" | "done" => {
                    if board.is_empty() || id.is_empty() {
                        format!("usage: kanban {action} <board> <id> [to-col]")
                    } else {
                        let path = self2.board_path(&board);
                        let mut b = load_board(&path).await;
                        let target = if action == "done" {
                            let cols: Vec<String> = b["columns"].as_array()
                                .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
                                .unwrap_or_default();
                            cols.last().cloned().unwrap_or_else(|| "Done".to_string())
                        } else if col.is_empty() {
                            let cols: Vec<String> = b["columns"].as_array()
                                .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
                                .unwrap_or_default();
                            cols.get(1).cloned().or_else(|| cols.last().cloned()).unwrap_or_else(|| "In Progress".to_string())
                        } else {
                            col.clone()
                        };
                        let cards = b["cards"].as_object_mut().unwrap();
                        let mut found: Option<(String, Value)> = None;
                        for (from_col, map) in cards.iter_mut() {
                            if let Some(c) = map.as_object_mut().unwrap().remove(&id) {
                                found = Some((from_col.clone(), c));
                                break;
                            }
                        }
                        match found {
                            Some((from_col, c)) => {
                                if !b["cards"][&target].is_object() { b["cards"][&target] = json!({}); }
                                b["cards"][&target][&id] = c;
                                write_board(&path, &b).await;
                                format!("moved card `{id}` {from_col} → {target}")
                            }
                            None => format!("no card `{id}` on board `{board}`"),
                        }
                    }
                }
                "show" => {
                    if board.is_empty() {
                        "usage: kanban show <board> [col]".to_string()
                    } else {
                        let b = load_board(&self2.board_path(&board)).await;
                        render_board(&b, &col)
                    }
                }
                "delete" => {
                    if board.is_empty() || id.is_empty() {
                        "usage: kanban delete <board> <id>".to_string()
                    } else {
                        let path = self2.board_path(&board);
                        let mut b = load_board(&path).await;
                        let mut removed = false;
                        if let Some(cards) = b["cards"].as_object_mut() {
                            for map in cards.values_mut() {
                                if map.as_object_mut().unwrap().remove(&id).is_some() {
                                    removed = true;
                                    break;
                                }
                            }
                        }
                        write_board(&path, &b).await;
                        if removed {
                            format!("deleted card `{id}` from `{board}`")
                        } else {
                            format!("no card `{id}` on board `{board}`")
                        }
                    }
                }
                other => format!("unknown action {other:?} — see `kanban` help"),
            };
            ok_outcome("", "kanban", out, started.elapsed().as_millis() as u64)
        })
    }
}

fn default_board() -> Value {
    json!({
        "columns": ["To Do", "In Progress", "Done"],
        "cards": {},
        "seq": 0
    })
}

async fn load_board(path: &PathBuf) -> Value {
    match fs::read_to_string(path).await {
        Ok(s) => json_parse(&s).unwrap_or_else(|_| default_board()),
        Err(_) => default_board(),
    }
}

async fn write_board(path: &PathBuf, b: &Value) {
    let _ = fs::write(path, serde_json::to_string_pretty(b).unwrap_or_default()).await;
}

fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

fn json_parse(s: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(s)
}

fn render_board(b: &Value, filter: &str) -> String {
    let cols: Vec<String> = b["columns"].as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let cards = b["cards"].as_object().cloned().unwrap_or_default();
    let mut out = String::new();
    let mut total = 0usize;
    for col in &cols {
        if !filter.is_empty() && col != filter {
            continue;
        }
        let map = cards.get(col).and_then(|v| v.as_object()).cloned().unwrap_or_default();
        let mut ids: Vec<&String> = map.keys().collect();
        ids.sort();
        out.push_str(&format!("\n── {col} ({}) ──\n", ids.len()));
        if ids.is_empty() {
            out.push_str("   (empty)\n");
        }
        for id in &ids {
            let c = &map[id.as_str()];
            let title = c["title"].as_str().unwrap_or("?");
            let body = c["body"].as_str().unwrap_or("");
            let extra = if body.is_empty() { String::new() } else { format!(" — {body}") };
            out.push_str(&format!("   [{id}] {title}{extra}\n"));
        }
        total += ids.len();
    }
    if filter.is_empty() {
        out.insert_str(0, &format!("total {total} cards"));
    } else {
        out.insert_str(0, &format!("cards in `{filter}`: {total}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_board_has_three_columns() {
        let b = default_board();
        assert_eq!(b["columns"].as_array().unwrap().len(), 3);
        assert_eq!(b["columns"][2], "Done");
    }

    #[test]
    fn render_empty_board() {
        let b = default_board();
        let s = render_board(&b, "");
        assert!(s.contains("total 0 cards"));
        assert!(s.contains("To Do"));
        assert!(s.contains("Done"));
    }

    #[test]
    fn render_with_filter_limits() {
        let mut b = default_board();
        b["cards"]["To Do"]["c1"] = json!({"title": "t", "body": "", "created": "x"});
        b["cards"]["Done"]["c2"] = json!({"title": "t2", "body": "b", "created": "x"});
        let s = render_board(&b, "Done");
        assert!(s.contains("cards in `Done`: 1"));
        assert!(s.contains("[c2] t2 — b"));
        assert!(!s.contains("To Do"));
    }
}