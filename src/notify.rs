//! Уведомления Windows на острове. Программе без пакета Windows не даёт слушать уведомления напрямую, поэтому
//! читаем базу центра уведомлений (wpndatabase.db) только для чтения и раз в пару секунд смотрим, что пришло.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::settings;

static RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug)]
pub struct Toast {
    pub app: String,
    pub aumid: String,
    pub title: String,
    pub body: String,
}

fn db_path() -> std::path::PathBuf {
    let local = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_default();
    local.join(r"Microsoft\Windows\Notifications\wpndatabase.db")
}

/// Включает или выключает слежение (по настройке).
pub fn apply(enabled: bool) {
    if enabled && !RUNNING.swap(true, Ordering::SeqCst) {
        std::thread::Builder::new().name("OmniFlex notifications".into()).spawn(run).ok();
    }
}

fn run() {
    let mut last: i64 = -1;
    while settings::read(|s| s.island.notifications) {
        if let Err(e) = poll(&mut last) {
            crate::util::log_error(format!("уведомления: {e}"));
            std::thread::sleep(Duration::from_secs(30));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    RUNNING.store(false, Ordering::SeqCst);
}

fn poll(last: &mut i64) -> rusqlite::Result<()> {
    use rusqlite::{Connection, OpenFlags};
    let db = Connection::open_with_flags(db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    if *last < 0 {
        // Что было до запуска — не показываем
        *last = db.query_row("SELECT IFNULL(MAX(Id), 0) FROM Notification", [], |r| r.get(0))?;
        return Ok(());
    }
    let mut stmt = db.prepare(
        "SELECT n.Id, n.Payload, h.PrimaryId FROM Notification n JOIN NotificationHandler h ON n.HandlerId = h.RecordId \
         WHERE n.Id > ?1 AND n.Type = 'toast' ORDER BY n.Id",
    )?;
    let rows = stmt.query_map([*last], |r| {
        let id: i64 = r.get(0)?;
        let payload: Vec<u8> = r.get::<_, Option<Vec<u8>>>(1)?.unwrap_or_default();
        let aumid: String = r.get::<_, Option<String>>(2)?.unwrap_or_default();
        Ok((id, payload, aumid))
    })?;
    let excluded: Vec<String> = settings::read(|s| s.island.notifications_excluded.iter().map(|e| e.to_lowercase()).collect());
    for row in rows {
        let (id, payload, aumid) = row?;
        *last = (*last).max(id);
        let Some(toast) = parse(&payload, &aumid) else { continue };
        let name = toast.app.to_lowercase();
        if excluded.iter().any(|e| !e.is_empty() && (name.contains(e) || toast.aumid.to_lowercase().contains(e))) {
            continue;
        }
        crate::shell::post(move || crate::island::card(crate::island::Card::notification(toast)));
    }
    Ok(())
}

/// Имя программы для уведомления: из реестра (для программ без пакета), иначе по AUMID.
pub fn app_name(aumid: &str) -> String {
    let key = format!(r"Software\Classes\AppUserModelId\{aumid}");
    if let Some(name) = crate::win::reg_read_string(windows::Win32::System::Registry::HKEY_CURRENT_USER, &key, "DisplayName")
        .or_else(|| crate::win::reg_read_string(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, &key, "DisplayName"))
    {
        if !name.starts_with('@') && !name.trim().is_empty() {
            return name;
        }
    }
    crate::media::player::app_name(aumid)
}

fn parse(payload: &[u8], aumid: &str) -> Option<Toast> {
    let xml = String::from_utf8_lossy(payload);
    let re = regex::Regex::new(r"(?s)<text[^>]*>(.*?)</text>").ok()?;
    let texts: Vec<String> = re
        .captures_iter(&xml)
        .map(|c| html_escape::decode_html_entities(c.get(1).map(|m| m.as_str()).unwrap_or("")).trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if texts.is_empty() {
        return None;
    }
    Some(Toast {
        app: app_name(aumid),
        aumid: aumid.to_string(),
        title: texts[0].clone(),
        body: texts[1..].join("\n"),
    })
}

/// Открыть программу уведомления.
pub fn open_app(aumid: &str) {
    if aumid.is_empty() {
        return;
    }
    let _ = std::process::Command::new("explorer.exe").arg(format!("shell:AppsFolder\\{aumid}")).spawn();
}
