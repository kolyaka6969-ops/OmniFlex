//! Связь с острова с внешним миром через именованный канал: ИИ-агенты в терминале (Claude Code и Codex)
//! сообщают, что делают, а команды из командной строки (`OmniFlex.exe --click-through toggle`) доходят
//! до запущенной копии. Хуки агентов запускают `OmniFlex.exe --agent …`: эта короткая копия передаёт событие
//! и печатает ответ (решение по разрешению для Claude Code).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::i18n::{tr, trs};

fn pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    format!(r"\\.\pipe\OmniFlex.{}", crate::media::lyrics::safe_file_name(&user))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Working,
    Waiting,
    Done,
}

#[derive(Clone, Debug)]
pub struct Agent {
    pub name: String,
    pub status: Status,
    pub text: String,
    pub at: std::time::Instant,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static WAITERS: Mutex<Option<HashMap<u64, Sender<String>>>> = Mutex::new(None);

/// Ответ с острова на вопрос агента.
pub fn reply(id: u64, answer: String) {
    if let Ok(mut w) = WAITERS.lock() {
        if let Some(tx) = w.get_or_insert_with(HashMap::new).remove(&id) {
            let _ = tx.send(answer);
        }
    }
}

// ---------- Сервер в запущенной копии ----------

pub fn start_server() {
    std::thread::Builder::new()
        .name("OmniFlex pipe".into())
        .spawn(|| {
            let name = crate::util::wide(&pipe_name());
            loop {
                let handle = unsafe {
                    CreateNamedPipeW(
                        PCWSTR(name.as_ptr()),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        PIPE_UNLIMITED_INSTANCES,
                        65536,
                        65536,
                        0,
                        None,
                    )
                };
                if handle.is_invalid() {
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                let connected = unsafe { ConnectNamedPipe(handle, None) };
                // Клиент мог подключиться между созданием и ожиданием — это тоже успех (ERROR_PIPE_CONNECTED)
                let ok = connected.is_ok() || connected.as_ref().err().map(|e| e.code().0 as u32 == 0x8007_0217).unwrap_or(false);
                let file = unsafe { std::fs::File::from_raw_handle(handle.0 as _) };
                if !ok {
                    continue;
                }
                std::thread::spawn(move || serve(file));
            }
        })
        .ok();
}

fn serve(file: std::fs::File) {
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut file = reader.into_inner();
    let Ok(req) = serde_json::from_str::<serde_json::Value>(&line) else { return };
    let answer = handle(&req);
    let _ = file.write_all(answer.as_bytes());
    let _ = file.write_all(b"\n");
    let _ = file.flush();
}

fn handle(req: &serde_json::Value) -> String {
    match req["cmd"].as_str().unwrap_or("") {
        "click-through" => {
            let value = req["value"].as_str().unwrap_or("toggle").to_lowercase();
            crate::settings::update(|s| {
                s.island.click_through = match value.as_str() {
                    "on" | "1" | "true" => true,
                    "off" | "0" | "false" => false,
                    _ => !s.island.click_through,
                }
            });
            "{}".into()
        }
        "show" => {
            crate::ui::show_main(crate::ui::Page::Settings);
            "{}".into()
        }
        "agent" => {
            let agent = req["agent"].as_str().unwrap_or("");
            let event = &req["event"];
            if agent == "codex" {
                codex_event(event);
                "{}".into()
            } else {
                claude_event(event)
            }
        }
        _ => "{}".into(),
    }
}

fn update(name: &str, status: Option<Status>, text: String) {
    let name = name.to_string();
    crate::shell::post(move || crate::island::agent_update(name, status, text));
}

fn short(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > n {
        s.chars().take(n).collect::<String>() + "…"
    } else {
        s
    }
}

/// Последний ответ Claude из журнала сеанса.
fn last_assistant_text(transcript: &str) -> Option<String> {
    let mut file = std::fs::File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(512 * 1024);
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    buf.push_str(&String::from_utf8_lossy(&bytes));
    for line in buf.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        if let Some(parts) = v["message"]["content"].as_array() {
            let text: Vec<&str> = parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect();
            if !text.is_empty() {
                return Some(text.join("\n"));
            }
        }
    }
    None
}

fn claude_event(e: &serde_json::Value) -> String {
    const NAME: &str = "Claude Code";
    match e["hook_event_name"].as_str().unwrap_or("") {
        "UserPromptSubmit" => {
            update(NAME, Some(Status::Working), short(e["prompt"].as_str().unwrap_or(""), 80));
            "{}".into()
        }
        "Notification" => {
            let message = e["message"].as_str().unwrap_or("").to_string();
            let kind = e["notification_type"].as_str().unwrap_or("");
            update(NAME, Some(Status::Waiting), short(&message, 80));
            if kind != "permission_prompt" {
                crate::shell::post(move || crate::island::card(crate::island::Card::agent(NAME, message)));
            }
            "{}".into()
        }
        "Stop" => {
            let text = e["transcript_path"].as_str().and_then(last_assistant_text).unwrap_or_default();
            update(NAME, Some(Status::Done), short(&text, 80));
            if !text.is_empty() {
                crate::shell::post(move || crate::island::card(crate::island::Card::agent(NAME, text)));
            }
            "{}".into()
        }
        "SessionEnd" => {
            update(NAME, None, String::new());
            "{}".into()
        }
        "PermissionRequest" => permission(e),
        _ => "{}".into(),
    }
}

/// Claude Code просит разрешения: карточка с кнопками, ответ уходит обратно в хук.
fn permission(e: &serde_json::Value) -> String {
    let tool = e["tool_name"].as_str().unwrap_or("").to_string();
    let input = e["tool_input"].clone();
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = channel();
    if let Ok(mut w) = WAITERS.lock() {
        w.get_or_insert_with(HashMap::new).insert(id, tx);
    }
    let question = if tool == "AskUserQuestion" { input["questions"].get(0).cloned() } else { None };
    let card = match &question {
        Some(q) => {
            let options: Vec<String> = q["options"].as_array().map(|a| a.iter().filter_map(|o| o["label"].as_str().map(String::from)).collect()).unwrap_or_default();
            (trs("Claude Code спрашивает", "Claude Code asks"), q["question"].as_str().unwrap_or("").to_string(), options)
        }
        None => {
            let detail = input["command"]
                .as_str()
                .or_else(|| input["file_path"].as_str())
                .or_else(|| input["url"].as_str())
                .map(String::from)
                .unwrap_or_else(|| short(&input.to_string(), 160));
            (trs(format!("Claude Code: {tool}"), format!("Claude Code: {tool}")), detail, Vec::new())
        }
    };
    update("Claude Code", Some(Status::Waiting), short(&card.1, 80));
    crate::shell::post(move || crate::island::card(crate::island::Card::agent_question(id, card.0, card.1, card.2)));
    let answer = rx.recv_timeout(Duration::from_secs(110)).unwrap_or_default();
    if let Ok(mut w) = WAITERS.lock() {
        w.get_or_insert_with(HashMap::new).remove(&id);
    }
    crate::shell::post(move || crate::island::agent_card_closed(id));
    let decision = if answer == "allow" {
        serde_json::json!({"behavior": "allow"})
    } else if answer == "deny" {
        serde_json::json!({"behavior": "deny", "message": tr("Отклонено на острове OmniFlex", "Denied on the OmniFlex island")})
    } else if let Some(label) = answer.strip_prefix("option:") {
        let mut updated = input.clone();
        let q = question.as_ref().and_then(|q| q["question"].as_str()).unwrap_or("").to_string();
        updated["answers"] = serde_json::json!({ q: label });
        serde_json::json!({"behavior": "allow", "updatedInput": updated})
    } else {
        // Не ответили — Claude Code спросит в терминале, как обычно
        update("Claude Code", Some(Status::Waiting), String::new());
        return "{}".into();
    };
    update("Claude Code", Some(Status::Working), String::new());
    serde_json::json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}}).to_string()
}

fn codex_event(e: &serde_json::Value) {
    const NAME: &str = "Codex";
    if e["type"].as_str() == Some("agent-turn-complete") {
        let text = e["last-assistant-message"].as_str().unwrap_or("").to_string();
        update(NAME, Some(Status::Done), short(&text, 80));
        if !text.is_empty() {
            crate::shell::post(move || crate::island::card(crate::island::Card::agent(NAME, text)));
        }
    }
}

// ---------- Короткая копия: передать и выйти ----------

fn send(request: serde_json::Value, wait: Duration) -> Option<String> {
    let name = pipe_name();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut file = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            Ok(f) => break f,
            Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    };
    let _ = wait;
    file.write_all(format!("{request}\n").as_bytes()).ok()?;
    let mut reader = BufReader::new(file);
    let mut answer = String::new();
    reader.read_line(&mut answer).ok()?;
    Some(answer.trim().to_string())
}

/// Обработка аргументов командной строки до запуска программы. true — это была команда, пора выходить.
pub fn run_cli(args: &[String]) -> bool {
    let Some(pos) = args.iter().position(|a| a == "--agent" || a == "--click-through") else { return false };
    if args[pos] == "--click-through" {
        let value = args.get(pos + 1).cloned().unwrap_or_else(|| "toggle".into());
        send(serde_json::json!({"cmd": "click-through", "value": value}), Duration::from_secs(2));
        return true;
    }
    let agent = args.get(pos + 1).cloned().unwrap_or_default();
    let event: serde_json::Value = if agent == "codex" {
        // Codex передаёт событие последним аргументом
        args.last().and_then(|a| serde_json::from_str(a).ok()).unwrap_or_default()
    } else {
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        serde_json::from_str(&input).unwrap_or_default()
    };
    if let Some(answer) = send(serde_json::json!({"cmd": "agent", "agent": agent, "event": event}), Duration::from_secs(120)) {
        if answer != "{}" && !answer.is_empty() {
            let mut out = std::io::stdout();
            let _ = out.write_all(answer.as_bytes());
            let _ = out.flush();
        }
    }
    true
}

// ---------- Подключение одним нажатием ----------

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default()
}

fn exe() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

fn claude_settings() -> PathBuf {
    home().join(".claude").join("settings.json")
}

fn codex_config() -> PathBuf {
    home().join(".codex").join("config.toml")
}

const CLAUDE_EVENTS: [(&str, u32); 5] = [("UserPromptSubmit", 5), ("Notification", 5), ("Stop", 10), ("PermissionRequest", 120), ("SessionEnd", 5)];

fn is_ours(command: &str) -> bool {
    command.contains("OmniFlex") && command.contains("--agent")
}

pub fn claude_connected() -> bool {
    std::fs::read_to_string(claude_settings()).map(|t| t.contains("--agent claude")).unwrap_or(false)
}

pub fn codex_connected() -> bool {
    std::fs::read_to_string(codex_config()).map(|t| t.contains("\"--agent\"") && t.contains("OmniFlex")).unwrap_or(false)
}

/// Добавляет хуки OmniFlex в настройки Claude Code, не трогая остальные.
pub fn connect_claude(on: bool) -> Result<(), String> {
    let path = claude_settings();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
    let mut json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if !json.is_object() {
        json = serde_json::json!({});
    }
    let command = format!("\"{}\" --agent claude", exe());
    let hooks = json.as_object_mut().unwrap().entry("hooks").or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let hooks = hooks.as_object_mut().unwrap();
    for (event, timeout) in CLAUDE_EVENTS {
        let list = hooks.entry(event).or_insert_with(|| serde_json::json!([]));
        if !list.is_array() {
            *list = serde_json::json!([]);
        }
        let arr = list.as_array_mut().unwrap();
        // Убираем свои старые записи (exe мог переехать)
        arr.retain(|entry| !entry["hooks"].as_array().map(|h| h.iter().any(|x| is_ours(x["command"].as_str().unwrap_or("")))).unwrap_or(false));
        if on {
            arr.push(serde_json::json!({"matcher": "", "hooks": [{"type": "command", "command": command, "timeout": timeout}]}));
        }
    }
    hooks.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&home()));
    let backup = path.with_extension("json.omniflex-backup");
    if path.exists() && !backup.exists() {
        let _ = std::fs::copy(&path, &backup);
    }
    let out = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    std::fs::write(&path, out).map_err(|e| e.to_string())
}

/// Строка notify в config.toml Codex.
pub fn connect_codex(on: bool) -> Result<(), String> {
    let path = codex_config();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let ours = format!("notify = [{}, \"--agent\", \"codex\"]", serde_json::to_string(&exe()).unwrap_or_default());
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    // notify — ключ верхнего уровня: только до первой таблицы
    let first_table = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
    let existing = lines[..first_table].iter().position(|l| l.trim_start().starts_with("notify"));
    match (existing, on) {
        (Some(i), true) => {
            if !lines[i].contains("OmniFlex") {
                return Err(tr("В config.toml Codex уже есть своя строка notify — уберите её или подключите вручную", "Codex config.toml already has its own notify line").into());
            }
            lines[i] = ours;
        }
        (None, true) => lines.insert(0, ours),
        (Some(i), false) => {
            if lines[i].contains("OmniFlex") {
                lines.remove(i);
            }
        }
        (None, false) => {}
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&home()));
    std::fs::write(&path, lines.join("\n") + "\n").map_err(|e| e.to_string())
}
