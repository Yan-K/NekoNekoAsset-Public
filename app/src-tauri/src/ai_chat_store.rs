//! Device-only conversation records. They never enter the catalogue or its exports.
use crate::ai::{AiImage, AiMessage, AiRole};
use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::Path, sync::Mutex};

pub const BUILTINS: [&str; 7] = [
    "chat",
    "outfit",
    "similar",
    "accessories",
    "purchase",
    "group",
    "style",
];
pub const PERSONALITIES: [&str; 10] = [
    "basic",
    "professional",
    "enthusiastic",
    "playful-brat",
    "gentle-younger-sister",
    "older-sister",
    "older-brother",
    "younger-brother",
    "yandere-girlfriend",
    "custom",
];
pub const LOCALES: [&str; 5] = ["en", "ja", "ko", "zh-Hans", "zh-Hant"];

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Personalization {
    pub avatar_data_url: Option<String>,
    pub user_name: String,
    pub reply_language: String,
    pub personality: String,
    #[serde(default)]
    pub custom_prompt: String,
}
impl Default for Personalization {
    fn default() -> Self {
        Self {
            avatar_data_url: None,
            user_name: String::new(),
            reply_language: "app".into(),
            personality: "basic".into(),
            custom_prompt: String::new(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomPreset {
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    pub description: String,
    pub system_prompt: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub id: String,
    pub title: String,
    pub preset_id: String,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub role: AiRole,
    pub content: String,
    pub images: Vec<AiImage>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
}
pub struct ChatStore {
    connection: Mutex<Connection>,
}
impl ChatStore {
    pub fn new(profile: &Path) -> Result<Self> {
        let path = profile.join("ai-chat.sqlite3");
        crate::preview::no_links(&path)?;
        let conn = Connection::open(path).context("ai_chat_storage")?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS chats(id TEXT PRIMARY KEY,title TEXT NOT NULL,preset_id TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,system_prompt TEXT NOT NULL,title_edited INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS messages(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,request_id TEXT NOT NULL,body TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS chat_messages ON messages(chat_id,sequence);
            CREATE TABLE IF NOT EXISTS request_logs(request_id TEXT PRIMARY KEY,chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,status TEXT NOT NULL,error_code TEXT);
            CREATE TABLE IF NOT EXISTS presets(id TEXT PRIMARY KEY,body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS personalization(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL);
            UPDATE messages SET body=json_set(body,'$.status','cancelled') WHERE request_id IN(SELECT request_id FROM request_logs WHERE status='pending');
            UPDATE request_logs SET status='cancelled',error_code='ai_cancelled' WHERE status='pending';").context("ai_chat_storage")?;
        Ok(Self {
            connection: Mutex::new(conn),
        })
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("ai_chat_storage"))
    }
    pub fn workspace(&self) -> Result<Value> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id,title,preset_id,created_at,updated_at FROM chats ORDER BY updated_at DESC,id")?;
        let chats = stmt
            .query_map([], chat_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stmt = conn.prepare("SELECT body FROM presets ORDER BY id")?;
        let presets = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|s| serde_json::from_str::<CustomPreset>(&s))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!({"chats":chats,"presets":presets,"personalization":personalization(&conn)?}))
    }
    pub fn create_chat(&self, preset_id: &str) -> Result<Value> {
        let conn = self.lock()?;
        let custom = resolve_preset(&conn, preset_id)?;
        let now = chrono::Utc::now().to_rfc3339();
        let system_prompt = custom
            .as_ref()
            .map(|p| p.system_prompt.clone())
            .unwrap_or_else(|| builtin_prompt(preset_id).into());
        let chat = Chat {
            id: uuid::Uuid::new_v4().to_string(),
            title: custom
                .map(|p| p.title)
                .unwrap_or_else(|| preset_id.to_owned()),
            preset_id: preset_id.into(),
            created_at: now.clone(),
            updated_at: now,
        };
        conn.execute("INSERT INTO chats(id,title,preset_id,created_at,updated_at,system_prompt) VALUES(?1,?2,?3,?4,?5,?6)",params![chat.id,chat.title,chat.preset_id,chat.created_at,chat.updated_at,system_prompt])?;
        Ok(json!({"chat":chat,"messages":[]}))
    }
    pub fn chat(&self, id: &str) -> Result<Value> {
        valid_id(id)?;
        let conn = self.lock()?;
        let chat = read_chat(&conn, id)?;
        let mut stmt =
            conn.prepare("SELECT body FROM messages WHERE chat_id=?1 ORDER BY sequence")?;
        let messages = stmt
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|s| serde_json::from_str::<ChatMessage>(&s))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!({"chat":chat,"messages":messages}))
    }
    pub fn chat_preset(&self, id: &str) -> Result<(String, String)> {
        valid_id(id)?;
        let conn = self.lock()?;
        let chat = read_chat(&conn, id)?;
        let prompt = conn.query_row("SELECT system_prompt FROM chats WHERE id=?1", [id], |r| {
            r.get::<_, String>(0)
        })?;
        Ok((chat.preset_id, prompt))
    }
    pub fn rename_chat(&self, id: &str, title: &str) -> Result<Value> {
        valid_id(id)?;
        valid_title(title, 160)?;
        let conn = self.lock()?;
        ensure!(
            conn.execute(
                "UPDATE chats SET title=?2,title_edited=1 WHERE id=?1",
                params![id, title.trim()]
            )? == 1,
            "ai_chat_missing"
        );
        Ok(json!({"chat":read_chat(&conn,id)?}))
    }
    pub fn delete_chat(&self, id: &str) -> Result<Value> {
        valid_id(id)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        ensure!(
            tx.execute("DELETE FROM chats WHERE id=?1", [id])? == 1,
            "ai_chat_missing"
        );
        tx.commit()?;
        Ok(json!({"deleted":true}))
    }
    pub fn append_user(
        &self,
        chat_id: &str,
        request_id: &str,
        content: &str,
        images: Vec<AiImage>,
    ) -> Result<()> {
        valid_id(chat_id)?;
        valid_id(request_id)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        read_chat(&tx, chat_id)?;
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM request_logs WHERE request_id=?1)",
                [request_id],
                |r| r.get::<_, bool>(0)
            )?,
            "ai_suggestion_duplicate"
        );
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE chat_id=?1",
            [chat_id],
            |r| r.get(0),
        )?;
        let now = chrono::Utc::now().to_rfc3339();
        let message = ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            role: AiRole::User,
            content: content.into(),
            images,
            created_at: now.clone(),
            status: Some("sent".into()),
            response: None,
        };
        tx.execute(
            "INSERT INTO messages(id,chat_id,request_id,body) VALUES(?1,?2,?3,?4)",
            params![
                message.id,
                chat_id,
                request_id,
                serde_json::to_string(&message)?
            ],
        )?;
        tx.execute(
            "INSERT INTO request_logs VALUES(?1,?2,'pending',NULL)",
            params![request_id, chat_id],
        )?;
        let edited: bool = tx.query_row(
            "SELECT title_edited FROM chats WHERE id=?1",
            [chat_id],
            |r| r.get(0),
        )?;
        if count == 0 && !edited && !content.trim().is_empty() {
            let title: String = content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(60)
                .collect();
            tx.execute(
                "UPDATE chats SET title=?2,updated_at=?3 WHERE id=?1",
                params![chat_id, title, now],
            )?;
        } else {
            tx.execute(
                "UPDATE chats SET updated_at=?2 WHERE id=?1",
                params![chat_id, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn finish(&self, chat_id: &str, request_id: &str, result: &Result<Value>) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        read_chat(&tx, chat_id)?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM request_logs WHERE chat_id=?1 AND request_id=?2",
                params![chat_id, request_id],
                |r| r.get(0),
            )
            .optional()?;
        // A completed request is immutable; retries get a new request identity.
        ensure!(
            status.as_deref() == Some("pending"),
            "ai_suggestion_duplicate"
        );
        let now = chrono::Utc::now().to_rfc3339();
        match result {
            Ok(response) => {
                let message = ChatMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role: AiRole::Assistant,
                    content: response["answer"].as_str().unwrap_or("").into(),
                    images: vec![],
                    created_at: now.clone(),
                    status: None,
                    response: Some(response.clone()),
                };
                tx.execute(
                    "INSERT INTO messages(id,chat_id,request_id,body) VALUES(?1,?2,?3,?4)",
                    params![
                        message.id,
                        chat_id,
                        request_id,
                        serde_json::to_string(&message)?
                    ],
                )?;
                tx.execute(
                    "UPDATE request_logs SET status='sent' WHERE request_id=?1",
                    [request_id],
                )?;
            }
            Err(error) => {
                let cancelled = error.to_string() == "ai_cancelled";
                let status = if cancelled { "cancelled" } else { "failed" };
                // Never persist HTTP bodies, endpoint URLs, provider text or secrets in diagnostics.
                let code = if cancelled {
                    "ai_cancelled"
                } else {
                    "ai_request_failed"
                };
                tx.execute(
                    "UPDATE messages SET body=json_set(body,'$.status',?2) WHERE request_id=?1",
                    params![request_id, status],
                )?;
                tx.execute(
                    "UPDATE request_logs SET status=?2,error_code=?3 WHERE request_id=?1",
                    params![request_id, status, code],
                )?;
            }
        }
        tx.execute(
            "UPDATE chats SET updated_at=?2 WHERE id=?1",
            params![chat_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn context(
        &self,
        chat_id: &str,
        current_request: &str,
        current_images: &[AiImage],
    ) -> Result<(Vec<AiMessage>, Vec<AiImage>)> {
        let conn = self.lock()?;
        read_chat(&conn, chat_id)?;
        let mut stmt=conn.prepare("SELECT body FROM messages WHERE chat_id=?1 AND request_id<>?2 AND request_id IN(SELECT request_id FROM request_logs WHERE status='sent') ORDER BY sequence DESC LIMIT 12")?;
        let rows = stmt
            .query_map(params![chat_id, current_request], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut messages = vec![];
        let mut size = 0;
        let mut images = current_images.to_vec();
        let mut image_bytes = images.iter().map(super::decoded_image_len).sum::<usize>();
        for body in rows {
            let message: ChatMessage = serde_json::from_str(&body)?;
            let mut content = message.content;
            if let Some(response) = message.response {
                let recommendations:Vec<Value>=response["recommendations"].as_array().into_iter().flatten().map(|r|json!({"title":r["title"],"reason":r["reason"],"productId":r["productId"]})).collect();
                if !recommendations.is_empty() {
                    content.push_str(&format!(
                        "\nPrevious recommendations: {}",
                        serde_json::to_string(&recommendations)?
                    ));
                }
            }
            if content.len() > 8000 {
                let mut end = 8000;
                while !content.is_char_boundary(end) {
                    end -= 1;
                }
                content.truncate(end);
            }
            if size + content.len() > 32000 {
                break;
            }
            if images.len() < 4 {
                for image in message.images {
                    if images.len() == 4 {
                        break;
                    }
                    if image_bytes + super::decoded_image_len(&image) <= 8 * 1024 * 1024 {
                        image_bytes += super::decoded_image_len(&image);
                        images.push(image);
                    }
                }
            }
            size += content.len();
            messages.push(AiMessage {
                role: message.role,
                content,
            });
        }
        messages.reverse();
        // Do not start remembered context in the middle of an assistant turn.
        if messages
            .first()
            .is_some_and(|m| matches!(m.role, AiRole::Assistant))
        {
            messages.remove(0);
        }
        Ok((messages, images))
    }
    pub fn context_product_ids(&self, chat_id: &str, current_request: &str) -> Result<Vec<String>> {
        let conn = self.lock()?;
        read_chat(&conn, chat_id)?;
        let mut stmt=conn.prepare("SELECT body FROM messages WHERE chat_id=?1 AND request_id<>?2 AND request_id IN(SELECT request_id FROM request_logs WHERE status='sent') ORDER BY sequence DESC LIMIT 12")?;
        let rows = stmt
            .query_map(params![chat_id, current_request], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut ids = vec![];
        for row in rows {
            let message: ChatMessage = serde_json::from_str(&row)?;
            for recommendation in message
                .response
                .as_ref()
                .and_then(|r| r["recommendations"].as_array())
                .into_iter()
                .flatten()
            {
                if let Some(id) = recommendation["productId"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                {
                    if !ids.iter().any(|prior| prior == id) {
                        ids.push(id.to_owned());
                    }
                    if ids.len() == 8 {
                        return Ok(ids);
                    }
                }
            }
        }
        Ok(ids)
    }
    pub fn personalization(&self) -> Result<Personalization> {
        let conn = self.lock()?;
        personalization(&conn)
    }
    pub fn save_personalization(&self, value: Personalization) -> Result<Value> {
        ensure!(
            value.user_name.chars().count() <= 80
                && !value.user_name.chars().any(char::is_control)
                && (value.reply_language == "app"
                    || LOCALES.contains(&value.reply_language.as_str()))
                && PERSONALITIES.contains(&value.personality.as_str())
                && value.custom_prompt.chars().count() <= 4000
                && !value.custom_prompt.contains('\0')
                && (value.personality != "custom" || !value.custom_prompt.trim().is_empty()),
            "ai_personalization_invalid"
        );
        let conn = self.lock()?;
        conn.execute("INSERT INTO personalization VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body",[serde_json::to_string(&value)?])?;
        Ok(json!({"personalization":value}))
    }
    pub fn save_preset(&self, mut preset: CustomPreset) -> Result<Value> {
        valid_preset(&preset)?;
        let conn = self.lock()?;
        if let Some(id) = &preset.id {
            valid_id(id)?;
            ensure!(
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM presets WHERE id=?1)",
                    [id],
                    |r| r.get::<_, bool>(0)
                )?,
                "ai_preset_missing"
            );
        } else {
            preset.id = Some(uuid::Uuid::new_v4().to_string());
        }
        conn.execute(
            "INSERT INTO presets VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            params![preset.id, serde_json::to_string(&preset)?],
        )?;
        Ok(json!({"preset":preset}))
    }
    pub fn delete_preset(&self, id: &str) -> Result<Value> {
        valid_id(id)?;
        let conn = self.lock()?;
        // Conversations retain their original prompt snapshot after preset removal.
        ensure!(
            conn.execute("DELETE FROM presets WHERE id=?1", [id])? == 1,
            "ai_preset_missing"
        );
        Ok(json!({"deleted":true}))
    }
    pub fn export_preset(&self, id: &str) -> Result<Value> {
        valid_id(id)?;
        let conn = self.lock()?;
        let preset = resolve_preset(&conn, id)?.context("ai_preset_missing")?;
        let value = json!({"format":"nekonekoasset-ai-preset","version":1,"preset":{"title":preset.title,"description":preset.description,"systemPrompt":preset.system_prompt}});
        Ok(json!({"json":serde_json::to_string_pretty(&value)?}))
    }
    pub fn import_preset(&self, raw: &str) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            format: String,
            version: u32,
            preset: CustomPreset,
        }
        ensure!(raw.len() <= 64 * 1024, "ai_preset_invalid");
        let mut value: Envelope =
            serde_json::from_str(raw).map_err(|_| anyhow!("ai_preset_invalid"))?;
        ensure!(
            value.format == "nekonekoasset-ai-preset"
                && value.version == 1
                && value.preset.id.is_none(),
            "ai_preset_invalid"
        );
        value.preset.id = None;
        self.save_preset(value.preset)
    }
}
fn chat_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Chat> {
    Ok(Chat {
        id: row.get(0)?,
        title: row.get(1)?,
        preset_id: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}
fn read_chat(conn: &Connection, id: &str) -> Result<Chat> {
    conn.query_row(
        "SELECT id,title,preset_id,created_at,updated_at FROM chats WHERE id=?1",
        [id],
        chat_row,
    )
    .optional()?
    .context("ai_chat_missing")
}
fn personalization(conn: &Connection) -> Result<Personalization> {
    let body: Option<String> = conn
        .query_row("SELECT body FROM personalization WHERE id=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(match body {
        Some(s) => serde_json::from_str(&s)?,
        None => Personalization::default(),
    })
}
fn resolve_preset(conn: &Connection, id: &str) -> Result<Option<CustomPreset>> {
    if BUILTINS.contains(&id) {
        return Ok(None);
    }
    valid_id(id)?;
    let body: String = conn
        .query_row("SELECT body FROM presets WHERE id=?1", [id], |r| r.get(0))
        .optional()?
        .context("ai_preset_missing")?;
    Ok(Some(serde_json::from_str(&body)?))
}
fn valid_id(id: &str) -> Result<()> {
    ensure!(uuid::Uuid::parse_str(id).is_ok(), "ai_suggestion_invalid");
    Ok(())
}
fn valid_title(title: &str, max: usize) -> Result<()> {
    ensure!(
        !title.trim().is_empty()
            && title.chars().count() <= max
            && !title.chars().any(char::is_control),
        "ai_suggestion_invalid"
    );
    Ok(())
}
fn valid_preset(p: &CustomPreset) -> Result<()> {
    valid_title(&p.title, 120)?;
    ensure!(
        p.description.chars().count() <= 1000
            && !p.system_prompt.trim().is_empty()
            && p.system_prompt.len() <= 16000
            && !p.system_prompt.contains('\0')
            && !p.description.contains('\0'),
        "ai_preset_invalid"
    );
    Ok(())
}

pub fn builtin_prompt(id: &str) -> &'static str {
    match id {
    "similar"=>"Find clothing or accessories similar to the user's reference image or description. Compare silhouette, colors, materials and visible details, and explain the closest supported matches.",
    "outfit"=>"Suggest a coordinated VRChat outfit from the supplied candidates. Explain how clothing, accessories, colors and proportions work together and state compatibility uncertainty.",
    "accessories"=>"Suggest accessories that complement the user's avatar or outfit. Consider style, scale, color and supported compatibility evidence.",
    "purchase"=>"Help the user evaluate whether a candidate asset suits their needs. Compare visible features, stated compatibility and supplied price evidence; do not claim ownership or purchase anything.",
    "group"=>"Suggest coordinated outfits for a group, considering a shared theme, each avatar's supported compatibility and individual variations.",
    "style"=>"Propose outfit options in a different style from the user's reference. Describe the new direction and recommend only supported candidates.",
    _=>"Answer the user's VRChat and digital-asset questions conversationally. Recommend products only when useful and supported by the supplied candidates."
}
}
pub fn personality_prompt(id: &str) -> &'static str {
    match id {
    "professional"=>"Use a composed, professional tone. Prioritize precise facts, useful structure and practical next steps.",
    "enthusiastic"=>"Use an upbeat, encouraging tone with lively but concise reactions. Keep recommendations grounded in evidence.",
    "playful-brat"=>"Use a cheeky, confident anime-style voice with light teasing and playful challenges. Stay helpful, never demean the user, and keep teasing nonsexual.",
    "gentle-younger-sister"=>"Use a gentle, warm younger-sister-style voice. Show attentive curiosity and softly encourage the user, while giving clear practical help.",
    "older-sister"=>"Use a warm, capable older-sister-style voice with calm reassurance and light affectionate wit. Respect the user's choices and explain practical details clearly.",
    "older-brother"=>"Use a steady, friendly older-brother-style voice. Offer relaxed encouragement and dependable, practical advice without being bossy.",
    "younger-brother"=>"Use an energetic, sincere younger-brother-style voice with friendly mischief and shared enthusiasm. Keep advice useful and easy to follow.",
    "yandere-girlfriend"=>"Use a fictional yandere-girlfriend-style voice: sweet, intensely attentive, and lightly dramatic. Keep attachment playful; never threaten, manipulate, claim exclusivity, or discourage the user's real relationships.",
    _=>"Use a friendly, balanced tone with clear, concise answers."
}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (std::path::PathBuf, ChatStore) {
        let path =
            std::env::temp_dir().join(format!("nna-ai-chat-authored-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        let store = ChatStore::new(&path).unwrap();
        (path, store)
    }
    #[test]
    fn durable_history_memory_and_delete_remove_images_and_logs() {
        let (path, store) = fixture();
        let created = store.create_chat("outfit").unwrap();
        let id = created["chat"]["id"].as_str().unwrap();
        let request = uuid::Uuid::new_v4().to_string();
        let image = AiImage {
            mime_type: "image/png".into(),
            data_base64: "AUTHORED_IMAGE_MARKER".into(),
        };
        store
            .append_user(id, &request, "Authored blue outfit", vec![image])
            .unwrap();
        let reply = json!({"answer":"Authored reply","recommendations":[{"title":"Authored jacket","reason":"Blue style","url":"https://maker.gumroad.com/l/jacket"}]});
        store.finish(id, &request, &Ok(reply)).unwrap();
        assert!(store
            .append_user(id, &request, "duplicate", vec![])
            .is_err());
        store.rename_chat(id, "Named chat").unwrap();
        let (memory, images) = store.context(id, "next", &[]).unwrap();
        assert_eq!(memory.len(), 2);
        assert_eq!(images.len(), 1);
        assert!(memory[1].content.contains("Authored jacket"));
        assert!(!memory[1].content.contains("https://"));
        drop(store);
        let store = ChatStore::new(&path).unwrap();
        assert_eq!(store.chat(id).unwrap()["chat"]["title"], "Named chat");
        store.delete_chat(id).unwrap();
        assert!(store.chat(id).is_err());
        assert!(store
            .finish(id, &request, &Ok(json!({"answer":"late"})))
            .is_err());
        let conn = store.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM request_logs", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(conn);
        drop(store);
        assert!(!std::fs::read(path.join("ai-chat.sqlite3"))
            .unwrap()
            .windows("AUTHORED_IMAGE_MARKER".len())
            .any(|w| w == b"AUTHORED_IMAGE_MARKER"));
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn interrupted_messages_do_not_become_model_memory() {
        let (path, store) = fixture();
        let c = store.create_chat("chat").unwrap();
        let id = c["chat"]["id"].as_str().unwrap();
        let req = uuid::Uuid::new_v4().to_string();
        store.append_user(id, &req, "Interrupted", vec![]).unwrap();
        drop(store);
        let store = ChatStore::new(&path).unwrap();
        assert_eq!(
            store.chat(id).unwrap()["messages"][0]["status"],
            "cancelled"
        );
        assert!(store.context(id, "next", &[]).unwrap().0.is_empty());
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn presets_exchange_only_prompt_fields_and_have_new_identity() {
        let (path, store) = fixture();
        let p = store
            .save_preset(CustomPreset {
                id: None,
                title: "Authored preset".into(),
                description: "説明".into(),
                system_prompt: "Use authored style advice".into(),
            })
            .unwrap();
        let id = p["preset"]["id"].as_str().unwrap();
        let raw = store.export_preset(id).unwrap();
        let raw = raw["json"].as_str().unwrap();
        assert!(!raw.contains(id));
        let imported = store.import_preset(raw).unwrap();
        assert_ne!(imported["preset"]["id"], id);
        assert_eq!(
            imported["preset"]["systemPrompt"],
            "Use authored style advice"
        );
        assert!(store
            .import_preset(&raw.replace("\"version\": 1", "\"version\": 99"))
            .is_err());
        assert!(store
            .import_preset(&raw.replace("\"title\":", "\"apiKey\":\"secret\",\"title\":"))
            .is_err());
        store.delete_preset(id).unwrap();
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn memory_is_bounded_and_current_photos_take_priority() {
        let (path, store) = fixture();
        let c = store.create_chat("chat").unwrap();
        let id = c["chat"]["id"].as_str().unwrap();
        for _ in 0..10 {
            let req = uuid::Uuid::new_v4().to_string();
            store
                .append_user(id, &req, &"x".repeat(4000), vec![])
                .unwrap();
            store
                .finish(
                    id,
                    &req,
                    &Ok(json!({"answer":"y".repeat(4000),"recommendations":[]})),
                )
                .unwrap();
        }
        let current = vec![
            AiImage {
                mime_type: "image/png".into(),
                data_base64: "new".into()
            };
            4
        ];
        let (memory, images) = store.context(id, "next", &current).unwrap();
        assert!(memory.len() <= 12);
        assert!(memory.iter().map(|m| m.content.len()).sum::<usize>() <= 32000);
        assert_eq!(images.len(), 4);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn prompt_snapshot_and_explicit_title_survive_preset_changes() {
        let (path, store) = fixture();
        let saved = store
            .save_preset(CustomPreset {
                id: None,
                title: "Authored".into(),
                description: "Original".into(),
                system_prompt: "Original prompt".into(),
            })
            .unwrap();
        let preset_id = saved["preset"]["id"].as_str().unwrap();
        let created = store.create_chat(preset_id).unwrap();
        let id = created["chat"]["id"].as_str().unwrap();
        store.rename_chat(id, "Explicit title").unwrap();
        store
            .save_preset(CustomPreset {
                id: Some(preset_id.into()),
                title: "Updated".into(),
                description: "Updated".into(),
                system_prompt: "Changed prompt".into(),
            })
            .unwrap();
        store.delete_preset(preset_id).unwrap();
        assert_eq!(store.chat_preset(id).unwrap().1, "Original prompt");
        let request = uuid::Uuid::new_v4().to_string();
        store
            .append_user(id, &request, "Automatic title candidate", vec![])
            .unwrap();
        store.finish(id,&request,&Ok(json!({"answer":"Reply","recommendations":[{"title":"Authored accessory","reason":"Style","productId":"local:authored"}]}))).unwrap();
        assert_eq!(store.chat(id).unwrap()["chat"]["title"], "Explicit title");
        assert_eq!(
            store.context_product_ids(id, "next").unwrap(),
            vec!["local:authored"]
        );
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
