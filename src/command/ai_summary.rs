//! AI-powered diff summaries for the gargo diff/compare pages.
//!
//! This module is provider-agnostic at the call site but currently implements a
//! single backend: OpenAI's Chat Completions API, reached over plain HTTP via
//! [`ureq`] (gargo has no async HTTP client dependency, and these calls run on
//! the blocking pool anyway).
//!
//! Two concerns live here:
//! - [`AiConfig`]: the non-secret runtime settings threaded from `config.toml`
//!   into the server thread. The API key itself is never carried here — it is
//!   read from the environment at request time.
//! - [`AiSummaryStore`]: a SQLite cache, modelled on
//!   [`crate::command::diff_viewed::ViewedStore`], so an unchanged diff is never
//!   re-summarised (the OpenAI call is the only billed cost).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, Result};

/// Most recent chat turns forwarded to the provider. Older turns are dropped so
/// a long session can't grow token cost/latency without bound.
const MAX_CHAT_HISTORY: usize = 24;

/// Non-secret AI settings threaded from `config.toml` to the server thread.
///
/// `Clone`/`Debug` so it can ride inside [`crate::command::gargo_server`]'s
/// `Start` command. The API key is intentionally absent — it is read from the
/// environment named by `api_key_env` only when a request is served.
#[derive(Debug, Clone)]
pub struct AiConfig {
    pub enabled: bool,
    pub provider: String,
    pub model: String,
    pub api_key_env: String,
    pub max_tokens: u32,
    /// Largest diff (bytes) sent to the model; larger ones are refused.
    pub max_diff_bytes: usize,
    /// Natural language for the generated summary (e.g. `English`, `Japanese`).
    pub language: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        // Keep a single source of truth for the default values: they live on
        // `PluginGargoServerAiConfig` (the config-file type) and are derived
        // here via `From`, so the two structs can never drift apart.
        Self::from(&crate::config::PluginGargoServerAiConfig::default())
    }
}

impl From<&crate::config::PluginGargoServerAiConfig> for AiConfig {
    fn from(c: &crate::config::PluginGargoServerAiConfig) -> Self {
        Self {
            enabled: c.enabled,
            provider: c.provider.clone(),
            model: c.model.clone(),
            api_key_env: c.api_key_env.clone(),
            max_tokens: c.max_tokens,
            max_diff_bytes: c.max_diff_bytes,
            language: c.language.clone(),
        }
    }
}

/// Stable cache key for a diff's content. Cheap, deterministic, not crypto.
pub fn diff_hash(diff_text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    diff_text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Build the system prompt, instructing the model to answer in `language`.
fn system_prompt(language: &str) -> String {
    format!(
        "You are a senior code reviewer. Summarise the given unified git diff for \
a teammate who is about to review the pull request. Be concise. Cover: (1) the \
overall purpose of the change in one or two sentences, (2) the key changes as a \
short bullet list grouped by area/file, and (3) any risks or things a reviewer \
should look at closely. Use Markdown. Do not restate the diff line by line. \
Write the entire response in {language} (keep code identifiers, file paths, and \
branch names verbatim)."
    )
}

/// One turn of a chat conversation, as sent by the client.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// System prompt for the chat endpoint: the diff is the grounding context.
fn chat_system_prompt(language: &str, diff_text: &str) -> String {
    format!(
        "You are a senior code reviewer helping a teammate review a pull request. \
The unified git diff under review is given below. Answer the user's questions \
about it accurately and concisely, grounding every answer in the diff; if \
something is not shown in the diff, say so rather than guessing. Use Markdown, \
and keep code identifiers, file paths, and branch names verbatim. Respond in \
{language}.\n\n--- DIFF UNDER REVIEW ---\n{diff_text}"
    )
}

fn read_api_key(config: &AiConfig) -> std::result::Result<String, String> {
    std::env::var(&config.api_key_env).map_err(|_| {
        format!(
            "{} is not set. Run `export {}=sk-...` in the shell that launches gargo.",
            config.api_key_env, config.api_key_env
        )
    })
}

/// Summarise `diff_text` via the configured provider.
///
/// Returns `Ok(markdown_summary)` or `Err(human_readable_message)`. Blocking;
/// callers run it on the blocking pool.
pub fn generate_summary(config: &AiConfig, diff_text: &str) -> std::result::Result<String, String> {
    let api_key = read_api_key(config)?;
    match config.provider.as_str() {
        "openai" => {
            let messages = serde_json::json!([
                { "role": "system", "content": system_prompt(&config.language) },
                { "role": "user", "content": diff_text },
            ]);
            call_openai(
                &api_key,
                config,
                messages,
                "OpenAI returned an empty summary",
            )
        }
        other => Err(format!("unsupported AI provider: {other}")),
    }
}

/// Answer a chat conversation grounded in `diff_text`.
///
/// `messages` is the client-side conversation (oldest first). The diff is
/// injected as a leading system message. Blocking; run on the blocking pool.
pub fn generate_chat(
    config: &AiConfig,
    diff_text: &str,
    messages: &[ChatMessage],
) -> std::result::Result<String, String> {
    let api_key = read_api_key(config)?;
    match config.provider.as_str() {
        "openai" => {
            let mut msgs = vec![serde_json::json!({
                "role": "system",
                "content": chat_system_prompt(&config.language, diff_text),
            })];
            // Bound the forwarded history: every turn re-sends the whole
            // conversation plus the diff system prompt, so an unbounded session
            // grows token cost/latency without limit. Keep only the most recent
            // turns (the diff, not old chatter, is the important grounding).
            let recent = messages
                .len()
                .checked_sub(MAX_CHAT_HISTORY)
                .map_or(messages, |start| &messages[start..]);
            for m in recent {
                // Only forward known roles so a malformed client can't inject
                // arbitrary message types.
                let role = match m.role.as_str() {
                    "user" | "assistant" => m.role.as_str(),
                    _ => "user",
                };
                msgs.push(serde_json::json!({ "role": role, "content": m.content }));
            }
            call_openai(
                &api_key,
                config,
                serde_json::Value::Array(msgs),
                "OpenAI returned an empty reply",
            )
        }
        other => Err(format!("unsupported AI provider: {other}")),
    }
}

/// Shared HTTP agent for provider calls, built once with explicit timeouts so a
/// hung provider can't pin a blocking-pool thread indefinitely (the pool is
/// shared with git diffs and file I/O). Reusing one agent also keeps the TLS
/// connection warm across calls instead of a fresh handshake per request.
fn http_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(60))
            .timeout_write(Duration::from_secs(30))
            .build()
    })
}

/// Low-level OpenAI Chat Completions call shared by summary and chat.
fn call_openai(
    api_key: &str,
    config: &AiConfig,
    messages: serde_json::Value,
    empty_msg: &str,
) -> std::result::Result<String, String> {
    let body = serde_json::json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "messages": messages,
    });

    let response = http_agent()
        .post("https://api.openai.com/v1/chat/completions")
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_json(body);

    let json: serde_json::Value = match response {
        Ok(resp) => resp
            .into_json()
            .map_err(|e| format!("failed to read OpenAI response: {e}"))?,
        // ureq surfaces non-2xx as Error::Status; extract the API error message.
        Err(ureq::Error::Status(code, resp)) => {
            let detail = resp
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(format!("OpenAI API error {code}: {detail}"));
        }
        Err(e) => return Err(format!("request to OpenAI failed: {e}")),
    };

    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| empty_msg.to_string())
}

/// SQLite-backed cache of generated summaries.
///
/// Modelled on [`crate::command::diff_viewed::ViewedStore`]: any open/IO error
/// degrades to a silent no-op so the server keeps working without persistence.
/// One row per `(repo_root, page, base_ref, compare_ref)`; a row is only a hit
/// while the stored `content_hash` and `model` still match the live request.
pub struct AiSummaryStore {
    conn: Mutex<Option<Connection>>,
}

impl Default for AiSummaryStore {
    fn default() -> Self {
        Self::open()
    }
}

impl AiSummaryStore {
    pub fn open() -> Self {
        Self::open_in_dir(&crate::config::app_data_dir())
    }

    pub fn open_in_dir(data_dir: &Path) -> Self {
        Self {
            conn: Mutex::new(Self::init(data_dir).ok()),
        }
    }

    fn init(data_dir: &Path) -> Result<Connection> {
        std::fs::create_dir_all(data_dir).ok();
        let conn = Connection::open(data_dir.join("ai_summary.db"))?;
        conn.busy_timeout(Duration::from_millis(3000))?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ai_summaries (
                repo_root    TEXT NOT NULL,
                page         TEXT NOT NULL,
                base_ref     TEXT NOT NULL,
                compare_ref  TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                model        TEXT NOT NULL,
                summary      TEXT NOT NULL,
                updated_at   INTEGER NOT NULL,
                PRIMARY KEY (repo_root, page, base_ref, compare_ref)
            )",
            [],
        )?;
        Ok(conn)
    }

    fn now_millis() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    /// Return the cached summary iff it matches the current `content_hash` and
    /// `model`; otherwise `None` (miss / stale).
    pub fn get(
        &self,
        repo_root: &str,
        page: &str,
        base_ref: &str,
        compare_ref: &str,
        content_hash: &str,
        model: &str,
    ) -> Option<String> {
        let guard = self.conn.lock().ok()?;
        let conn = guard.as_ref()?;
        conn.query_row(
            "SELECT summary FROM ai_summaries
             WHERE repo_root = ?1 AND page = ?2 AND base_ref = ?3 AND compare_ref = ?4
               AND content_hash = ?5 AND model = ?6",
            rusqlite::params![repo_root, page, base_ref, compare_ref, content_hash, model],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Insert or replace the cached summary for this context.
    #[allow(clippy::too_many_arguments)]
    pub fn set(
        &self,
        repo_root: &str,
        page: &str,
        base_ref: &str,
        compare_ref: &str,
        content_hash: &str,
        model: &str,
        summary: &str,
    ) -> Result<()> {
        let Ok(guard) = self.conn.lock() else {
            return Ok(());
        };
        let Some(conn) = guard.as_ref() else {
            return Ok(());
        };
        conn.execute(
            "INSERT INTO ai_summaries
                (repo_root, page, base_ref, compare_ref, content_hash, model, summary, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(repo_root, page, base_ref, compare_ref)
             DO UPDATE SET content_hash = ?5, model = ?6, summary = ?7, updated_at = ?8",
            rusqlite::params![
                repo_root,
                page,
                base_ref,
                compare_ref,
                content_hash,
                model,
                summary,
                Self::now_millis(),
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, AiSummaryStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = AiSummaryStore::open_in_dir(dir.path());
        (dir, store)
    }

    #[test]
    fn set_then_get_roundtrip() {
        let (_dir, store) = temp_store();
        store
            .set(
                "/repo",
                "compare",
                "main",
                "dev",
                "h1",
                "gpt-4o-mini",
                "summary text",
            )
            .unwrap();
        assert_eq!(
            store.get("/repo", "compare", "main", "dev", "h1", "gpt-4o-mini"),
            Some("summary text".to_string()),
        );
    }

    #[test]
    fn stale_hash_or_model_is_a_miss() {
        let (_dir, store) = temp_store();
        store
            .set("/repo", "compare", "main", "dev", "h1", "gpt-4o-mini", "s")
            .unwrap();
        // Different hash -> miss (diff changed).
        assert!(
            store
                .get("/repo", "compare", "main", "dev", "h2", "gpt-4o-mini")
                .is_none()
        );
        // Different model -> miss (re-summarise with the new model).
        assert!(
            store
                .get("/repo", "compare", "main", "dev", "h1", "gpt-4o")
                .is_none()
        );
    }

    #[test]
    fn set_replaces_existing_row() {
        let (_dir, store) = temp_store();
        store
            .set(
                "/repo",
                "compare",
                "main",
                "dev",
                "h1",
                "gpt-4o-mini",
                "old",
            )
            .unwrap();
        store
            .set(
                "/repo",
                "compare",
                "main",
                "dev",
                "h2",
                "gpt-4o-mini",
                "new",
            )
            .unwrap();
        assert!(
            store
                .get("/repo", "compare", "main", "dev", "h1", "gpt-4o-mini")
                .is_none()
        );
        assert_eq!(
            store.get("/repo", "compare", "main", "dev", "h2", "gpt-4o-mini"),
            Some("new".to_string()),
        );
    }

    #[test]
    fn noop_store_degrades_gracefully() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let store = AiSummaryStore::open_in_dir(file.path());
        assert!(
            store
                .get("/repo", "compare", "main", "dev", "h1", "m")
                .is_none()
        );
        assert!(
            store
                .set("/repo", "compare", "main", "dev", "h1", "m", "s")
                .is_ok()
        );
    }

    #[test]
    fn diff_hash_is_deterministic_and_sensitive() {
        assert_eq!(diff_hash("abc"), diff_hash("abc"));
        assert_ne!(diff_hash("abc"), diff_hash("abd"));
    }
}
