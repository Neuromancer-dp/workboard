mod chat;
pub mod db;
mod ollama;
pub mod search;

use chat::{ChatEvent, SendArgs, Shared};
use db::{now_ms, Conversation, Message, SearchHit};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{ipc::Channel, Manager, RunEvent, State};
use tokio::sync::Notify;

type Sh<'a> = State<'a, Arc<Shared>>;

fn err<E: ToString>(e: E) -> String {
    e.to_string()
}

fn port_of(sh: &Shared) -> u16 {
    sh.db.setting("ollama_port").and_then(|p| p.parse().ok()).filter(|p| *p > 0).unwrap_or(ollama::DEFAULT_PORT)
}

// ---------- settings / ollama ----------

const SETTING_KEYS: &[&str] = &["ollama_port", "last_model", "ctx", "web", "reduce_effects", "sidebar", "last_conversation"];

#[tauri::command]
fn get_settings(sh: Sh) -> Result<HashMap<String, String>, String> {
    sh.db.settings().map_err(err)
}

#[tauri::command]
fn set_setting(sh: Sh, key: String, value: String) -> Result<(), String> {
    if !SETTING_KEYS.contains(&key.as_str()) {
        return Err(format!("unknown setting {key}"));
    }
    if key == "ollama_port" {
        let p: u32 = value.trim().parse().map_err(|_| "port must be a number".to_string())?;
        if !(1..=65535).contains(&p) {
            return Err("port must be between 1 and 65535".into());
        }
        if p as u16 != port_of(&sh) {
            sh.ollama.shutdown(); // only stops an instance this app started
        }
        return sh.db.set_setting(&key, &p.to_string()).map_err(err);
    }
    sh.db.set_setting(&key, &value).map_err(err)
}

#[tauri::command]
async fn ollama_ensure(sh: Sh<'_>) -> Result<ollama::EnsureInfo, String> {
    let port = port_of(&sh);
    Ok(sh.ollama.ensure(port).await)
}

#[tauri::command]
async fn list_models(sh: Sh<'_>) -> Result<Vec<String>, String> {
    sh.ollama.tags(port_of(&sh)).await
}

#[tauri::command]
async fn model_vision(sh: Sh<'_>, model: String) -> Result<bool, String> {
    Ok(sh.ollama.has_vision(port_of(&sh), &model).await)
}

// ---------- chat ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SendIds {
    conversation_id: i64,
    user_message_id: i64,
    assistant_message_id: i64,
    title: String,
}

#[tauri::command]
async fn send_message(sh: Sh<'_>, mut args: SendArgs, on_event: Channel<ChatEvent>) -> Result<SendIds, String> {
    args.text = args.text.trim().to_string();
    if args.text.is_empty() && args.attachments.is_empty() {
        return Err("nothing to send".into());
    }
    if args.model.trim().is_empty() {
        return Err("no model selected".into());
    }
    args.num_ctx = args.num_ctx.clamp(512, 1_048_576);
    let cancel = Arc::new(Notify::new());
    let turn = {
        let mut active = sh.active.lock().unwrap_or_else(|e| e.into_inner());
        if active.is_some() {
            return Err("A reply is already being generated. Stop it first.".into());
        }
        // The user's message is on disk before anything else happens.
        let turn = sh.db.begin_turn(args.conversation_id, &args.model, &args.text, &args.attachments)?;
        *active = Some(chat::Active { message_id: turn.assistant_message_id, conversation_id: turn.conversation_id, cancel: cancel.clone() });
        turn
    };
    let ids = SendIds {
        conversation_id: turn.conversation_id,
        user_message_id: turn.user_message_id,
        assistant_message_id: turn.assistant_message_id,
        title: turn.title.clone(),
    };
    let port = port_of(&sh);
    let shared = sh.inner().clone();
    let emit: chat::Emit = Arc::new(move |e| {
        let _ = on_event.send(e);
    });
    tauri::async_runtime::spawn(chat::run_turn(shared, emit, turn, args, port, cancel));
    Ok(ids)
}

#[tauri::command]
fn cancel_generation(sh: Sh, message_id: i64) {
    if let Some(a) = sh.active.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if a.message_id == message_id {
            a.cancel.notify_one();
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveInfo {
    conversation_id: i64,
    message_id: i64,
}

#[tauri::command]
fn active_generation(sh: Sh) -> Option<ActiveInfo> {
    sh.active.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|a| ActiveInfo { conversation_id: a.conversation_id, message_id: a.message_id })
}

// ---------- history ----------

#[derive(Serialize)]
struct Loaded {
    conversation: Conversation,
    messages: Vec<Message>,
}

#[tauri::command]
fn list_conversations(sh: Sh) -> Result<Vec<Conversation>, String> {
    sh.db.list_conversations().map_err(err)
}
#[tauri::command]
fn list_trash(sh: Sh) -> Result<Vec<Conversation>, String> {
    sh.db.list_trash().map_err(err)
}
#[tauri::command]
fn search_conversations(sh: Sh, query: String) -> Result<Vec<SearchHit>, String> {
    sh.db.search(&query, 50).map_err(err)
}
#[tauri::command]
fn load_conversation(sh: Sh, id: i64) -> Result<Loaded, String> {
    let conversation = sh.db.conversation(id).map_err(err)?.ok_or("conversation not found")?;
    Ok(Loaded { conversation, messages: sh.db.messages(id).map_err(err)? })
}
#[tauri::command]
fn rename_conversation(sh: Sh, id: i64, title: String) -> Result<(), String> {
    sh.db.rename(id, &title).map_err(err)
}
#[tauri::command]
fn pin_conversation(sh: Sh, id: i64, pinned: bool) -> Result<(), String> {
    sh.db.set_pinned(id, pinned).map_err(err)
}
#[tauri::command]
fn delete_conversation(sh: Sh, id: i64) -> Result<(), String> {
    if let Some(a) = sh.active.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if a.conversation_id == id {
            return Err("Stop the running reply before deleting this chat.".into());
        }
    }
    sh.db.soft_delete(id).map_err(err)
}
#[tauri::command]
fn restore_conversation(sh: Sh, id: i64) -> Result<(), String> {
    sh.db.restore(id).map_err(err)
}
#[tauri::command]
fn purge_conversation(sh: Sh, id: i64) -> Result<(), String> {
    sh.db.purge(id).map_err(err)
}
#[tauri::command]
fn empty_trash(sh: Sh) -> Result<usize, String> {
    sh.db.empty_trash().map_err(err)
}
#[tauri::command]
fn attachment_bytes(sh: Sh, id: i64) -> Result<tauri::ipc::Response, String> {
    let (_, data) = sh.db.attachment_data(id).map_err(err)?.ok_or("attachment not found")?;
    Ok(tauri::ipc::Response::new(data))
}

// ---------- files ----------

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".partial");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).map_err(err)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        err(e)
    })
}

/// Paths come from the native save dialog; nothing else in the app calls these.
#[tauri::command]
fn save_text_file(path: String, content: String) -> Result<(), String> {
    write_atomic(Path::new(&path), content.as_bytes())
}
#[tauri::command]
fn export_all_json(sh: Sh, path: String) -> Result<(), String> {
    let v = sh.db.export_all().map_err(err)?;
    write_atomic(Path::new(&path), &serde_json::to_vec_pretty(&v).map_err(err)?)
}
#[tauri::command]
fn backup_database(sh: Sh, path: String) -> Result<(), String> {
    sh.db.backup_to(Path::new(&path))
}
#[tauri::command]
fn data_location(app: tauri::AppHandle) -> String {
    app.path().app_data_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let u = reqwest::Url::parse(&url).map_err(|_| "invalid link".to_string())?;
    if !matches!(u.scheme(), "http" | "https" | "mailto") {
        return Err(format!("refusing to open {} links", u.scheme()));
    }
    let mut cmd = Command::new("xdg-open");
    cmd.arg(u.as_str()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    ollama::clean_env(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("could not run xdg-open: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// ---------- app ----------

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db = db::Db::open(&dir.join("chat.db")).map_err(|e| format!("cannot open the chat database in {}: {e}", dir.display()))?;
            if let Ok(n) = db.recover_interrupted() {
                if n > 0 {
                    eprintln!("chat: recovered {n} interrupted reply draft(s)");
                }
            }
            let _ = db.purge_expired(now_ms());
            let sh = Arc::new(Shared { db, ollama: ollama::Ollama::new(), active: Mutex::new(None), flush_every: Duration::from_secs(2) });
            app.manage(sh.clone());
            let port = port_of(&sh);
            tauri::async_runtime::spawn(async move {
                let r = sh.ollama.ensure(port).await;
                if r.state == "failed" {
                    eprintln!("chat: {}", r.message);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            set_setting,
            ollama_ensure,
            list_models,
            model_vision,
            send_message,
            cancel_generation,
            active_generation,
            list_conversations,
            list_trash,
            search_conversations,
            load_conversation,
            rename_conversation,
            pin_conversation,
            delete_conversation,
            restore_conversation,
            purge_conversation,
            empty_trash,
            attachment_bytes,
            save_text_file,
            export_all_json,
            backup_database,
            data_location,
            open_external,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build the app");

    app.run(|app, event| match event {
        RunEvent::ExitRequested { api, .. } => {
            // Let a running reply stop and flush to SQLite before the process goes away.
            let sh = app.state::<Arc<Shared>>().inner().clone();
            let running = sh.active.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|a| a.cancel.clone());
            if let Some(c) = running {
                api.prevent_exit();
                c.notify_one();
                let handle = app.clone();
                std::thread::spawn(move || {
                    for _ in 0..60 {
                        if sh.active.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    handle.exit(0);
                });
            }
        }
        RunEvent::Exit => {
            app.state::<Arc<Shared>>().ollama.shutdown();
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::open_external;

    #[test]
    fn open_external_only_accepts_web_and_mail_links() {
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,hi",
            "ftp://x.example/",
            "not a url",
            "",
            "tauri://localhost/x",
            "//no-scheme.example",
        ] {
            assert!(open_external(bad.to_string()).is_err(), "{bad} must be refused");
        }
    }
}
