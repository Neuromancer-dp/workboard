//! SQLite persistence. Everything the user types or receives lands here before the UI shows it.

use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

pub const TRASH_DAYS: i64 = 30;

/// Ordered list of migrations. `PRAGMA user_version` is the number applied so far.
/// Never edit an entry that has shipped; append a new one.
const MIGRATIONS: &[&str] = &[
    // v1: base schema + FTS5 over message content
    r#"
    CREATE TABLE conversations (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        title       TEXT NOT NULL,
        model       TEXT NOT NULL DEFAULT '',
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        pinned      INTEGER NOT NULL DEFAULT 0,
        deleted_at  INTEGER
    );
    CREATE INDEX idx_conv_listing ON conversations(deleted_at, pinned, updated_at);

    CREATE TABLE messages (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        role            TEXT NOT NULL CHECK (role IN ('user','assistant')),
        content         TEXT NOT NULL DEFAULT '',
        thinking        TEXT NOT NULL DEFAULT '',
        status          TEXT NOT NULL DEFAULT 'complete',
        created_at      INTEGER NOT NULL,
        stats           TEXT,
        sources         TEXT
    );
    CREATE INDEX idx_msg_conv ON messages(conversation_id, id);

    CREATE TABLE attachments (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        message_id     INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
        kind           TEXT NOT NULL CHECK (kind IN ('image','pdf')),
        name           TEXT NOT NULL,
        mime           TEXT NOT NULL DEFAULT '',
        data           BLOB,
        extracted_text TEXT,
        pages          INTEGER,
        truncated      INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_att_msg ON attachments(message_id);

    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    CREATE VIRTUAL TABLE messages_fts USING fts5(
        content, content='messages', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
    END;
    CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
    END;
    CREATE TRIGGER messages_au AFTER UPDATE OF content ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', old.id, old.content);
        INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
    END;
    "#,
];

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub pinned: bool,
    pub deleted_at: Option<i64>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    #[serde(flatten)]
    pub conversation: Conversation,
    /// Matching text; hits are wrapped in \u{2} ... \u{3}.
    pub snippet: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentMeta {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub mime: String,
    pub pages: Option<i64>,
    pub chars: i64,
    pub truncated: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub conversation_id: i64,
    pub role: String,
    pub content: String,
    pub thinking: String,
    pub status: String,
    pub created_at: i64,
    pub stats: Option<Value>,
    pub sources: Option<Value>,
    pub attachments: Vec<AttachmentMeta>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct NewAttachment {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub mime: String,
    /// base64 payload (images)
    #[serde(default)]
    pub data_b64: Option<String>,
    /// extracted text (PDFs)
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub pages: Option<i64>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub conversation_id: i64,
    pub user_message_id: i64,
    pub assistant_message_id: i64,
    pub title: String,
}

/// One stored message as needed to rebuild the prompt context.
#[derive(Debug, Clone)]
pub struct HistTurn {
    pub role: String,
    pub content: String,
    pub docs: Vec<(String, String)>,
    pub images: Vec<Vec<u8>>,
}

pub struct Db {
    conn: Mutex<Connection>,
}

pub fn make_title(text: &str, atts: &[NewAttachment]) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let base = if flat.is_empty() { atts.first().map(|a| a.name.clone()).unwrap_or_default() } else { flat };
    if base.is_empty() {
        return "New chat".into();
    }
    let mut it = base.chars();
    let head: String = it.by_ref().take(50).collect();
    if it.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}

/// Turn free text into a safe FTS5 query: every word quoted, last one a prefix match.
pub fn fts_query(q: &str) -> Option<String> {
    let words: Vec<String> = q.split_whitespace().map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return None;
    }
    let last = words.len() - 1;
    Some(words.iter().enumerate().map(|(i, w)| if i == last { format!("\"{w}\"*") } else { format!("\"{w}\"") }).collect::<Vec<_>>().join(" "))
}

fn conv_from_row(r: &rusqlite::Row) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: r.get(0)?,
        title: r.get(1)?,
        model: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        pinned: r.get::<_, i64>(5)? != 0,
        deleted_at: r.get(6)?,
    })
}
const CONV_COLS: &str = "id, title, model, created_at, updated_at, pinned, deleted_at";

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Db> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_memory() -> rusqlite::Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> rusqlite::Result<Db> {
        // FULL: a committed user message survives power loss. Commits are rare (draft flush every ~2s).
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite in a bad state, so keep going.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---------- settings ----------
    pub fn settings(&self) -> rusqlite::Result<std::collections::HashMap<String, String>> {
        let c = self.c();
        let mut st = c.prepare("SELECT key, value FROM settings")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect()
    }
    pub fn set_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.c().execute("INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
        Ok(())
    }
    pub fn setting(&self, key: &str) -> Option<String> {
        self.c().query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    // ---------- writing a turn ----------
    /// Persist the user's message and an empty assistant draft in one transaction.
    /// After this returns, nothing the user typed can be lost.
    pub fn begin_turn(&self, conv: Option<i64>, model: &str, text: &str, atts: &[NewAttachment]) -> Result<Turn, String> {
        let mut c = self.c();
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let now = now_ms();
        let (cid, title) = match conv {
            None => {
                let title = make_title(text, atts);
                tx.execute("INSERT INTO conversations(title, model, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)", params![title, model, now])
                    .map_err(|e| e.to_string())?;
                (tx.last_insert_rowid(), title)
            }
            Some(id) => {
                let row: Option<(String, Option<i64>)> = tx
                    .query_row("SELECT title, deleted_at FROM conversations WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()
                    .map_err(|e| e.to_string())?;
                match row {
                    None => return Err("conversation not found".into()),
                    Some((_, Some(_))) => return Err("conversation is in the trash; restore it first".into()),
                    Some((title, None)) => {
                        tx.execute("UPDATE conversations SET model = ?2, updated_at = ?3 WHERE id = ?1", params![id, model, now]).map_err(|e| e.to_string())?;
                        (id, title)
                    }
                }
            }
        };
        tx.execute("INSERT INTO messages(conversation_id, role, content, status, created_at) VALUES (?1, 'user', ?2, 'complete', ?3)", params![cid, text, now])
            .map_err(|e| e.to_string())?;
        let uid = tx.last_insert_rowid();
        for a in atts {
            let blob = match &a.data_b64 {
                Some(b) => Some(base64::engine::general_purpose::STANDARD.decode(b).map_err(|e| format!("bad attachment data: {e}"))?),
                None => None,
            };
            tx.execute(
                "INSERT INTO attachments(message_id, kind, name, mime, data, extracted_text, pages, truncated) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![uid, a.kind, a.name, a.mime, blob, a.text, a.pages, a.truncated as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute("INSERT INTO messages(conversation_id, role, content, status, created_at) VALUES (?1, 'assistant', '', 'streaming', ?2)", params![cid, now])
            .map_err(|e| e.to_string())?;
        let aid = tx.last_insert_rowid();
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Turn { conversation_id: cid, user_message_id: uid, assistant_message_id: aid, title })
    }

    /// Flush the in-progress reply (called about every 2s while streaming).
    pub fn update_draft(&self, id: i64, content: &str, thinking: &str) -> rusqlite::Result<()> {
        self.c().execute("UPDATE messages SET content = ?2, thinking = ?3 WHERE id = ?1", params![id, content, thinking])?;
        Ok(())
    }

    pub fn set_sources(&self, id: i64, sources: &Value) -> rusqlite::Result<()> {
        self.c().execute("UPDATE messages SET sources = ?2 WHERE id = ?1", params![id, sources.to_string()])?;
        Ok(())
    }

    /// Final write for an assistant reply. An empty reply that was stopped is dropped (nothing to keep).
    pub fn finalize(&self, id: i64, content: &str, thinking: &str, status: &str, stats: &Value) -> rusqlite::Result<()> {
        let c = self.c();
        if content.is_empty() && thinking.is_empty() && status == "stopped" {
            c.execute("DELETE FROM messages WHERE id = ?1", [id])?;
            return Ok(());
        }
        c.execute(
            "UPDATE messages SET content = ?2, thinking = ?3, status = ?4, stats = ?5 WHERE id = ?1",
            params![id, content, thinking, status, stats.to_string()],
        )?;
        Ok(())
    }

    /// Startup recovery: drafts left in 'streaming' belong to a crashed run. Keep their text.
    pub fn recover_interrupted(&self) -> rusqlite::Result<usize> {
        let c = self.c();
        c.execute("DELETE FROM messages WHERE status = 'streaming' AND content = '' AND thinking = ''", [])?;
        c.execute("UPDATE messages SET status = 'interrupted' WHERE status = 'streaming'", [])
    }

    // ---------- reading ----------
    pub fn conversation(&self, id: i64) -> rusqlite::Result<Option<Conversation>> {
        self.c().query_row(&format!("SELECT {CONV_COLS} FROM conversations WHERE id = ?1"), [id], conv_from_row).optional()
    }

    pub fn list_conversations(&self) -> rusqlite::Result<Vec<Conversation>> {
        let c = self.c();
        let mut st = c.prepare(&format!("SELECT {CONV_COLS} FROM conversations WHERE deleted_at IS NULL ORDER BY pinned DESC, updated_at DESC, id DESC"))?;
        let rows = st.query_map([], conv_from_row)?;
        rows.collect()
    }

    pub fn list_trash(&self) -> rusqlite::Result<Vec<Conversation>> {
        let c = self.c();
        let mut st = c.prepare(&format!("SELECT {CONV_COLS} FROM conversations WHERE deleted_at IS NOT NULL ORDER BY deleted_at DESC"))?;
        let rows = st.query_map([], conv_from_row)?;
        rows.collect()
    }

    pub fn messages(&self, cid: i64) -> rusqlite::Result<Vec<Message>> {
        let c = self.c();
        let mut st = c.prepare(
            "SELECT id, conversation_id, role, content, thinking, status, created_at, stats, sources FROM messages WHERE conversation_id = ?1 ORDER BY id",
        )?;
        let mut att = c.prepare("SELECT id, kind, name, mime, pages, length(extracted_text), truncated FROM attachments WHERE message_id = ?1 ORDER BY id")?;
        let rows = st.query_map([cid], |r| {
            let stats: Option<String> = r.get(7)?;
            let sources: Option<String> = r.get(8)?;
            Ok(Message {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                thinking: r.get(4)?,
                status: r.get(5)?,
                created_at: r.get(6)?,
                stats: stats.and_then(|s| serde_json::from_str(&s).ok()),
                sources: sources.and_then(|s| serde_json::from_str(&s).ok()),
                attachments: vec![],
            })
        })?;
        let mut out = Vec::new();
        for m in rows {
            let mut m = m?;
            let a = att.query_map([m.id], |r| {
                Ok(AttachmentMeta {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    name: r.get(2)?,
                    mime: r.get(3)?,
                    pages: r.get(4)?,
                    chars: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    truncated: r.get::<_, i64>(6)? != 0,
                })
            })?;
            m.attachments = a.collect::<rusqlite::Result<_>>()?;
            out.push(m);
        }
        Ok(out)
    }

    pub fn attachment_data(&self, id: i64) -> rusqlite::Result<Option<(String, Vec<u8>)>> {
        self.c().query_row("SELECT mime, data FROM attachments WHERE id = ?1 AND data IS NOT NULL", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()
    }

    /// Stored messages before `before_id`, in order, ready to be turned into a prompt.
    /// Web-search text is never stored, so it cannot appear here.
    pub fn history_before(&self, cid: i64, before_id: i64) -> rusqlite::Result<Vec<HistTurn>> {
        let c = self.c();
        let mut st = c.prepare("SELECT id, role, content FROM messages WHERE conversation_id = ?1 AND id < ?2 ORDER BY id")?;
        let mut att = c.prepare("SELECT kind, name, data, extracted_text FROM attachments WHERE message_id = ?1 ORDER BY id")?;
        let msgs: Vec<(i64, String, String)> =
            st.query_map(params![cid, before_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::new();
        for (id, role, content) in msgs {
            let mut t = HistTurn { role, content, docs: vec![], images: vec![] };
            let rows =
                att.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<Vec<u8>>>(2)?, r.get::<_, Option<String>>(3)?)))?;
            for row in rows {
                let (kind, name, data, text) = row?;
                match (kind.as_str(), data, text) {
                    ("image", Some(d), _) => t.images.push(d),
                    ("pdf", _, Some(tx)) => t.docs.push((name, tx)),
                    _ => {}
                }
            }
            out.push(t);
        }
        Ok(out)
    }

    // ---------- conversation management ----------
    pub fn rename(&self, id: i64, title: &str) -> rusqlite::Result<()> {
        let t = title.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            return Ok(());
        }
        self.c().execute("UPDATE conversations SET title = ?2 WHERE id = ?1", params![id, t])?;
        Ok(())
    }
    pub fn set_pinned(&self, id: i64, pinned: bool) -> rusqlite::Result<()> {
        self.c().execute("UPDATE conversations SET pinned = ?2 WHERE id = ?1", params![id, pinned as i64])?;
        Ok(())
    }
    pub fn soft_delete(&self, id: i64) -> rusqlite::Result<()> {
        self.c().execute("UPDATE conversations SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL", params![id, now_ms()])?;
        Ok(())
    }
    pub fn restore(&self, id: i64) -> rusqlite::Result<()> {
        self.c().execute("UPDATE conversations SET deleted_at = NULL WHERE id = ?1", [id])?;
        Ok(())
    }
    /// Permanent delete. Only reachable from the Trash view behind a confirmation, or by 30-day expiry.
    pub fn purge(&self, id: i64) -> rusqlite::Result<()> {
        self.c().execute("DELETE FROM conversations WHERE id = ?1 AND deleted_at IS NOT NULL", [id])?;
        Ok(())
    }
    pub fn empty_trash(&self) -> rusqlite::Result<usize> {
        self.c().execute("DELETE FROM conversations WHERE deleted_at IS NOT NULL", [])
    }
    pub fn purge_expired(&self, now: i64) -> rusqlite::Result<usize> {
        let cutoff = now - TRASH_DAYS * 86_400_000;
        self.c().execute("DELETE FROM conversations WHERE deleted_at IS NOT NULL AND deleted_at < ?1", [cutoff])
    }

    pub fn search(&self, q: &str, limit: usize) -> rusqlite::Result<Vec<SearchHit>> {
        let c = self.c();
        let mut hits: Vec<SearchHit> = Vec::new();
        let like = format!("%{}%", q.trim().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        if !q.trim().is_empty() {
            let mut st = c.prepare(&format!(
                "SELECT {CONV_COLS} FROM conversations WHERE deleted_at IS NULL AND title LIKE ?1 ESCAPE '\\' ORDER BY pinned DESC, updated_at DESC LIMIT ?2"
            ))?;
            for r in st.query_map(params![like, limit as i64], conv_from_row)? {
                hits.push(SearchHit { conversation: r?, snippet: String::new() });
            }
        }
        if let Some(fq) = fts_query(q) {
            let mut st = c.prepare(
                "SELECT c.id, c.title, c.model, c.created_at, c.updated_at, c.pinned, c.deleted_at,
                        snippet(messages_fts, 0, char(2), char(3), '…', 12)
                 FROM messages_fts
                 JOIN messages m ON m.id = messages_fts.rowid
                 JOIN conversations c ON c.id = m.conversation_id
                 WHERE messages_fts MATCH ?1 AND c.deleted_at IS NULL
                 ORDER BY rank LIMIT ?2",
            )?;
            let rows = st.query_map(params![fq, (limit * 4) as i64], |r| Ok((conv_from_row(r)?, r.get::<_, String>(7)?)))?;
            for r in rows {
                let (conversation, snippet) = r?;
                if hits.len() >= limit {
                    break;
                }
                match hits.iter_mut().find(|h| h.conversation.id == conversation.id) {
                    Some(h) if h.snippet.is_empty() => h.snippet = snippet,
                    Some(_) => {}
                    None => hits.push(SearchHit { conversation, snippet }),
                }
            }
        }
        Ok(hits)
    }

    // ---------- export / backup ----------
    pub fn export_all(&self) -> rusqlite::Result<Value> {
        let convs: Vec<Conversation> = {
            let c = self.c();
            let mut st = c.prepare(&format!("SELECT {CONV_COLS} FROM conversations ORDER BY id"))?;
            let rows = st.query_map([], conv_from_row)?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut out = Vec::new();
        for cv in convs {
            let msgs = self.messages(cv.id)?;
            let mut ms = Vec::new();
            for m in msgs {
                let mut atts = Vec::new();
                for a in &m.attachments {
                    let c = self.c();
                    let (data, text): (Option<Vec<u8>>, Option<String>) =
                        c.query_row("SELECT data, extracted_text FROM attachments WHERE id = ?1", [a.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                    atts.push(json!({
                        "kind": a.kind, "name": a.name, "mime": a.mime, "pages": a.pages, "truncated": a.truncated,
                        "extractedText": text,
                        "dataBase64": data.map(|d| base64::engine::general_purpose::STANDARD.encode(d)),
                    }));
                }
                ms.push(json!({
                    "id": m.id, "role": m.role, "content": m.content, "thinking": m.thinking, "status": m.status,
                    "createdAt": m.created_at, "stats": m.stats, "sources": m.sources, "attachments": atts,
                }));
            }
            out.push(json!({
                "id": cv.id, "title": cv.title, "model": cv.model, "createdAt": cv.created_at, "updatedAt": cv.updated_at,
                "pinned": cv.pinned, "deletedAt": cv.deleted_at, "messages": ms,
            }));
        }
        Ok(json!({ "app": "chat", "format": 1, "exportedAt": now_ms(), "conversations": out }))
    }

    /// Consistent snapshot of the live database (WAL included) written next to `dest`, then renamed into place.
    pub fn backup_to(&self, dest: &Path) -> Result<(), String> {
        let mut tmp = dest.as_os_str().to_owned();
        tmp.push(".partial");
        let tmp = std::path::PathBuf::from(tmp);
        let _ = std::fs::remove_file(&tmp);
        {
            let c = self.c();
            c.execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()]).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, dest).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(name: &str) -> NewAttachment {
        NewAttachment {
            kind: "image".into(),
            name: name.into(),
            mime: "image/jpeg".into(),
            data_b64: Some(base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 4])),
            text: None,
            pages: None,
            truncated: false,
        }
    }
    fn pdf(name: &str, text: &str) -> NewAttachment {
        NewAttachment {
            kind: "pdf".into(),
            name: name.into(),
            mime: "application/pdf".into(),
            data_b64: None,
            text: Some(text.into()),
            pages: Some(3),
            truncated: false,
        }
    }

    #[test]
    fn migrations_are_versioned_and_idempotent() {
        let dir = std::env::temp_dir().join(format!("chat-test-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.db");
        {
            let db = Db::open(&p).unwrap();
            db.set_setting("a", "1").unwrap();
        }
        let db = Db::open(&p).unwrap(); // reopen: no re-run, data kept
        assert_eq!(db.setting("a").as_deref(), Some("1"));
        let v: i64 = db.c().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        let mode: String = db.c().pragma_query_value(None, "journal_mode", |r| r.get(0)).unwrap();
        assert_eq!(mode, "wal");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn create_append_and_read_back() {
        let db = Db::open_memory().unwrap();
        let t = db.begin_turn(None, "m1", "hello   world", &[img("a.png"), pdf("d.pdf", "doc text")]).unwrap();
        assert_eq!(t.title, "hello world");
        // user message is on disk before any reply exists
        let m = db.messages(t.conversation_id).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].role, "user");
        assert_eq!(m[0].content, "hello   world");
        assert_eq!(m[0].attachments.len(), 2);
        assert_eq!(m[0].attachments[1].chars, 8);
        assert_eq!(m[1].status, "streaming");
        let (mime, bytes) = db.attachment_data(m[0].attachments[0].id).unwrap().unwrap();
        assert_eq!((mime.as_str(), bytes), ("image/jpeg", vec![1, 2, 3, 4]));
        assert!(db.attachment_data(m[0].attachments[1].id).unwrap().is_none());

        let t2 = db.begin_turn(Some(t.conversation_id), "m2", "again", &[]).unwrap();
        assert_eq!(t2.conversation_id, t.conversation_id);
        assert_eq!(db.conversation(t.conversation_id).unwrap().unwrap().model, "m2");
        assert_eq!(db.messages(t.conversation_id).unwrap().len(), 4);
    }

    #[test]
    fn title_is_first_50_chars() {
        let long = "x".repeat(80);
        let t = make_title(&long, &[]);
        assert_eq!(t.chars().count(), 51);
        assert!(t.ends_with('…'));
        assert_eq!(make_title("", &[pdf("report.pdf", "t")]), "report.pdf");
        assert_eq!(make_title("  ", &[]), "New chat");
        assert_eq!(make_title("héllo wörld", &[]), "héllo wörld");
    }

    #[test]
    fn draft_flush_then_finalize() {
        let db = Db::open_memory().unwrap();
        let t = db.begin_turn(None, "m", "q", &[]).unwrap();
        db.update_draft(t.assistant_message_id, "partial", "thinking…").unwrap();
        let m = db.messages(t.conversation_id).unwrap();
        assert_eq!((m[1].content.as_str(), m[1].thinking.as_str(), m[1].status.as_str()), ("partial", "thinking…", "streaming"));
        db.finalize(t.assistant_message_id, "partial and more", "thinking…", "complete", &json!({"tokS": 12.5})).unwrap();
        let m = db.messages(t.conversation_id).unwrap();
        assert_eq!(m[1].status, "complete");
        assert_eq!(m[1].stats.as_ref().unwrap()["tokS"], 12.5);
    }

    #[test]
    fn stopping_with_nothing_drops_only_the_empty_draft() {
        let db = Db::open_memory().unwrap();
        let t = db.begin_turn(None, "m", "q", &[]).unwrap();
        db.finalize(t.assistant_message_id, "", "", "stopped", &json!({})).unwrap();
        let m = db.messages(t.conversation_id).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].role, "user");
    }

    #[test]
    fn crash_recovery_keeps_partial_text() {
        let db = Db::open_memory().unwrap();
        let a = db.begin_turn(None, "m", "q1", &[]).unwrap();
        db.update_draft(a.assistant_message_id, "half an ans", "").unwrap();
        let b = db.begin_turn(None, "m", "q2", &[]).unwrap(); // crashed before any token
        assert_eq!(db.recover_interrupted().unwrap(), 1);
        assert_eq!(db.messages(a.conversation_id).unwrap()[1].status, "interrupted");
        assert_eq!(db.messages(a.conversation_id).unwrap()[1].content, "half an ans");
        assert_eq!(db.messages(b.conversation_id).unwrap().len(), 1);
    }

    #[test]
    fn soft_delete_restore_and_purge() {
        let db = Db::open_memory().unwrap();
        let t = db.begin_turn(None, "m", "keep me", &[img("i.png")]).unwrap();
        db.finalize(t.assistant_message_id, "answer", "", "complete", &json!({})).unwrap();
        let id = t.conversation_id;

        db.soft_delete(id).unwrap();
        assert!(db.list_conversations().unwrap().is_empty());
        assert_eq!(db.list_trash().unwrap().len(), 1);
        assert!(db.search("answer", 10).unwrap().is_empty(), "trashed chats are not searchable");
        assert!(db.begin_turn(Some(id), "m", "x", &[]).is_err(), "cannot append to a trashed chat");
        assert_eq!(db.messages(id).unwrap().len(), 2, "soft delete keeps every row");

        db.restore(id).unwrap();
        assert_eq!(db.list_conversations().unwrap().len(), 1);
        assert!(db.list_trash().unwrap().is_empty());

        db.purge(id).unwrap(); // not in trash: refused
        assert_eq!(db.list_conversations().unwrap().len(), 1);

        db.soft_delete(id).unwrap();
        // not yet 30 days old: expiry leaves it alone
        assert_eq!(db.purge_expired(now_ms() + 29 * 86_400_000).unwrap(), 0);
        assert_eq!(db.purge_expired(now_ms() + 31 * 86_400_000).unwrap(), 1);
        assert!(db.messages(id).unwrap().is_empty());
        let n: i64 = db.c().query_row("SELECT count(*) FROM attachments", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "attachments cascade");
        let n: i64 = db.c().query_row("SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'answer'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "fts index cleaned");
    }

    #[test]
    fn listing_order_pinned_first() {
        let db = Db::open_memory().unwrap();
        let a = db.begin_turn(None, "m", "first", &[]).unwrap().conversation_id;
        std::thread::sleep(std::time::Duration::from_millis(3));
        let b = db.begin_turn(None, "m", "second", &[]).unwrap().conversation_id;
        assert_eq!(db.list_conversations().unwrap()[0].id, b);
        db.set_pinned(a, true).unwrap();
        assert_eq!(db.list_conversations().unwrap()[0].id, a);
        db.rename(a, "  renamed  chat ").unwrap();
        assert_eq!(db.conversation(a).unwrap().unwrap().title, "renamed chat");
    }

    #[test]
    fn full_text_search() {
        let db = Db::open_memory().unwrap();
        let a = db.begin_turn(None, "m", "How do I configure Hyprland monitors?", &[]).unwrap();
        db.finalize(a.assistant_message_id, "Use the monitor= keyword in hyprland.conf", "", "complete", &json!({})).unwrap();
        let b = db.begin_turn(None, "m", "Unrelated pasta recipe", &[]).unwrap();
        db.update_draft(b.assistant_message_id, "Boil water first", "").unwrap();

        let hits = db.search("hyprland", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation.id, a.conversation_id);
        assert!(hits[0].snippet.contains('\u{2}'), "match is marked: {:?}", hits[0].snippet);

        assert_eq!(db.search("monit", 10).unwrap().len(), 1, "prefix match on last word");
        assert_eq!(db.search("PASTA", 10).unwrap()[0].conversation.id, b.conversation_id, "case-insensitive, title match");
        assert_eq!(db.search("boil", 10).unwrap()[0].conversation.id, b.conversation_id, "drafts are searchable");
        assert!(db.search("nothingmatchesthis", 10).unwrap().is_empty());
        // hostile FTS syntax must not error
        for q in ["\"", "a AND", "(", "NEAR(", "*", "col:x", "50%", "_"] {
            db.search(q, 10).unwrap();
        }
        assert!(db.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn fts_query_builder() {
        assert_eq!(fts_query("foo bar").unwrap(), "\"foo\" \"bar\"*");
        assert_eq!(fts_query("a\"b OR").unwrap(), "\"ab\" \"OR\"*");
        assert!(fts_query("!!! ??").is_none());
    }

    #[test]
    fn history_rebuilds_context_without_the_draft() {
        let db = Db::open_memory().unwrap();
        let t1 = db.begin_turn(None, "m", "summarize", &[pdf("a.pdf", "PDFTEXT"), img("p.png")]).unwrap();
        db.finalize(t1.assistant_message_id, "summary", "", "complete", &json!({})).unwrap();
        let t2 = db.begin_turn(Some(t1.conversation_id), "m", "more", &[]).unwrap();
        let h = db.history_before(t2.conversation_id, t2.assistant_message_id).unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].docs, vec![("a.pdf".to_string(), "PDFTEXT".to_string())]);
        assert_eq!(h[0].images.len(), 1);
        assert_eq!(h[1].content, "summary");
        assert_eq!(h[2].content, "more");
    }

    #[test]
    fn export_and_backup() {
        let db = Db::open_memory().unwrap();
        let t = db.begin_turn(None, "m", "hi", &[img("a.png")]).unwrap();
        db.finalize(t.assistant_message_id, "yo", "", "complete", &json!({})).unwrap();
        db.soft_delete(t.conversation_id).unwrap();
        let j = db.export_all().unwrap();
        let conv = &j["conversations"][0];
        assert!(conv["deletedAt"].is_number(), "trashed chats are exported too");
        assert_eq!(conv["messages"][0]["attachments"][0]["dataBase64"], "AQIDBA==");
        assert_eq!(conv["messages"][1]["content"], "yo");

        let dir = std::env::temp_dir().join(format!("chat-bk-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("copy.db");
        db.backup_to(&dest).unwrap();
        let copy = Db::open(&dest).unwrap();
        assert_eq!(copy.list_trash().unwrap().len(), 1);
        assert_eq!(copy.messages(t.conversation_id).unwrap()[1].content, "yo");
        assert!(!dir.join("copy.db.partial").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
