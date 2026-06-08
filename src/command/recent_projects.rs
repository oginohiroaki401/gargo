use rusqlite::{Connection, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentProjectEntry {
    pub project_path: String,
    pub last_open_at: i64,
    pub last_edit_at: i64,
    pub last_open_file: Option<String>,
    pub last_edit_file: Option<String>,
}

pub struct RecentProjectsStore {
    conn: Option<Connection>,
}

impl Default for RecentProjectsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RecentProjectsStore {
    pub fn new() -> Self {
        let conn = Self::init_db(None).ok();
        Self { conn }
    }

    #[cfg(test)]
    pub fn new_with_data_dir(data_dir: PathBuf) -> Self {
        let conn = Self::init_db(Some(data_dir)).ok();
        Self { conn }
    }

    fn init_db(custom_data_dir: Option<PathBuf>) -> Result<Connection> {
        let data_dir = custom_data_dir.unwrap_or_else(Self::get_data_dir);
        std::fs::create_dir_all(&data_dir).ok();

        let db_path = data_dir.join("history.db");
        let conn = Connection::open(db_path)?;
        // The CLI and the web editor both write here concurrently, so use WAL and
        // a busy timeout to ride out the brief locks instead of erroring out.
        conn.busy_timeout(std::time::Duration::from_millis(1000))
            .ok();
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        Self::init_schema(&conn)?;
        Ok(conn)
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS recent_projects (
                project_path TEXT PRIMARY KEY,
                last_open_at INTEGER NOT NULL,
                last_edit_at INTEGER NOT NULL DEFAULT 0,
                last_open_file TEXT,
                last_edit_file TEXT
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_recent_projects_open_edit
             ON recent_projects(last_open_at DESC, last_edit_at DESC)",
            [],
        )?;

        // Per-file last-opened times, shared by the CLI and the web editor. Feeds
        // the Cmd+P picker's empty-query recency sort (see `get_file_open_times`).
        conn.execute(
            "CREATE TABLE IF NOT EXISTS file_opens (
                project_path TEXT NOT NULL,
                file_path TEXT NOT NULL,
                last_open_at INTEGER NOT NULL,
                PRIMARY KEY (project_path, file_path)
            )",
            [],
        )?;

        Ok(())
    }

    fn get_data_dir() -> PathBuf {
        if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
            PathBuf::from(xdg_data).join("gargo")
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".local/share/gargo")
        } else {
            PathBuf::from(".gargo")
        }
    }

    fn now_millis() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    fn normalize_project_root(project_root: &Path) -> PathBuf {
        std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf())
    }

    /// Upsert the per-file last-opened time. Best-effort: a failure here must not
    /// sink the surrounding project/file record, so the error is swallowed.
    fn touch_file_open(conn: &Connection, project: &str, rel: &str, timestamp: i64) {
        let _ = conn.execute(
            "INSERT INTO file_opens (project_path, file_path, last_open_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(project_path, file_path)
             DO UPDATE SET last_open_at = ?3",
            rusqlite::params![project, rel, timestamp],
        );
    }

    fn relative_file_path(project_root: &Path, file_path: &Path) -> Option<String> {
        if let Ok(rel) = file_path.strip_prefix(project_root) {
            return Some(rel.to_string_lossy().to_string());
        }

        let normalized_root = std::fs::canonicalize(project_root).ok()?;
        let normalized_file = std::fs::canonicalize(file_path).ok()?;
        let rel = normalized_file.strip_prefix(&normalized_root).ok()?;
        Some(rel.to_string_lossy().to_string())
    }

    pub fn record_project_open(
        &self,
        project_root: &Path,
        maybe_open_file: Option<&Path>,
    ) -> Result<()> {
        let conn = match &self.conn {
            Some(c) => c,
            None => return Ok(()),
        };

        let project = Self::normalize_project_root(project_root)
            .to_string_lossy()
            .to_string();
        let timestamp = Self::now_millis();
        let rel_open = maybe_open_file.and_then(|p| Self::relative_file_path(project_root, p));

        conn.execute(
            "INSERT INTO recent_projects (project_path, last_open_at, last_edit_at, last_open_file, last_edit_file)
             VALUES (?1, ?2, 0, ?3, NULL)
             ON CONFLICT(project_path)
             DO UPDATE SET
                last_open_at = ?2,
                last_open_file = COALESCE(?3, recent_projects.last_open_file)",
            rusqlite::params![project, timestamp, rel_open],
        )?;

        if let Some(rel) = &rel_open {
            Self::touch_file_open(conn, &project, rel, timestamp);
        }

        Ok(())
    }

    pub fn record_file_open(&self, project_root: &Path, file_path: &Path) -> Result<()> {
        let conn = match &self.conn {
            Some(c) => c,
            None => return Ok(()),
        };

        let Some(rel_open) = Self::relative_file_path(project_root, file_path) else {
            return Ok(());
        };
        let project = Self::normalize_project_root(project_root)
            .to_string_lossy()
            .to_string();
        let timestamp = Self::now_millis();

        conn.execute(
            "INSERT INTO recent_projects (project_path, last_open_at, last_edit_at, last_open_file, last_edit_file)
             VALUES (?1, ?2, 0, ?3, NULL)
             ON CONFLICT(project_path)
             DO UPDATE SET
                last_open_at = ?2,
                last_open_file = ?3",
            rusqlite::params![project, timestamp, rel_open],
        )?;

        Self::touch_file_open(conn, &project, &rel_open, timestamp);

        Ok(())
    }

    pub fn record_file_edit(&self, project_root: &Path, file_path: &Path) -> Result<()> {
        let conn = match &self.conn {
            Some(c) => c,
            None => return Ok(()),
        };

        let Some(rel_edit) = Self::relative_file_path(project_root, file_path) else {
            return Ok(());
        };
        let project = Self::normalize_project_root(project_root)
            .to_string_lossy()
            .to_string();
        let timestamp = Self::now_millis();

        conn.execute(
            "INSERT INTO recent_projects (project_path, last_open_at, last_edit_at, last_open_file, last_edit_file)
             VALUES (?1, 0, ?2, NULL, ?3)
             ON CONFLICT(project_path)
             DO UPDATE SET
                last_edit_at = ?2,
                last_edit_file = ?3",
            rusqlite::params![project, timestamp, rel_edit],
        )?;

        Self::touch_file_open(conn, &project, &rel_edit, timestamp);

        Ok(())
    }

    pub fn get_recent_projects(&self, limit: usize) -> Vec<RecentProjectEntry> {
        let conn = match &self.conn {
            Some(c) => c,
            None => return Vec::new(),
        };

        let mut stmt = match conn.prepare(
            "SELECT project_path, last_open_at, last_edit_at, last_open_file, last_edit_file
             FROM recent_projects
             ORDER BY last_open_at DESC, last_edit_at DESC
             LIMIT ?1",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };

        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            Ok(RecentProjectEntry {
                project_path: row.get(0)?,
                last_open_at: row.get(1)?,
                last_edit_at: row.get(2)?,
                last_open_file: row.get(3)?,
                last_edit_file: row.get(4)?,
            })
        });

        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn get_last_open_file(&self, project_root: &Path) -> Option<String> {
        let conn = self.conn.as_ref()?;
        let project = Self::normalize_project_root(project_root)
            .to_string_lossy()
            .to_string();

        let mut stmt = conn
            .prepare("SELECT last_open_file FROM recent_projects WHERE project_path = ?1")
            .ok()?;

        stmt.query_row(rusqlite::params![project], |row| row.get(0))
            .ok()
            .flatten()
    }

    /// Map of repo-relative path -> last-opened time (ms) for a project, across
    /// both the CLI and the web editor. Used to break ties in the Cmd+P picker.
    pub fn get_file_open_times(&self, project_root: &Path) -> HashMap<String, i64> {
        let mut map = HashMap::new();
        let Some(conn) = self.conn.as_ref() else {
            return map;
        };
        let project = Self::normalize_project_root(project_root)
            .to_string_lossy()
            .to_string();
        let Ok(mut stmt) =
            conn.prepare("SELECT file_path, last_open_at FROM file_opens WHERE project_path = ?1")
        else {
            return map;
        };
        let rows = stmt.query_map(rusqlite::params![project], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        });
        if let Ok(rows) = rows {
            for (path, ts) in rows.flatten() {
                map.insert(path, ts);
            }
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn record_open_edit_and_query_recent_projects() {
        let timestamp = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("gargo_recent_projects_{}", timestamp));
        fs::create_dir_all(&temp_dir).unwrap();

        let root_a = temp_dir.join("repo_a");
        let root_b = temp_dir.join("repo_b");
        fs::create_dir_all(root_a.join("src")).unwrap();
        fs::create_dir_all(root_b.join("src")).unwrap();
        let file_a = root_a.join("src").join("a.rs");
        let file_b = root_b.join("src").join("b.rs");
        fs::write(&file_a, "fn a() {}").unwrap();
        fs::write(&file_b, "fn b() {}").unwrap();

        let db_dir = temp_dir.join("db");
        let store = RecentProjectsStore::new_with_data_dir(db_dir);

        store.record_project_open(&root_a, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.record_file_open(&root_b, &file_b).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.record_file_edit(&root_b, &file_b).unwrap();

        let recent = store.get_recent_projects(10);
        assert_eq!(recent.len(), 2);
        assert!(recent[0].project_path.ends_with("repo_b"));
        assert_eq!(recent[0].last_open_file.as_deref(), Some("src/b.rs"));
        assert_eq!(recent[0].last_edit_file.as_deref(), Some("src/b.rs"));
        assert!(recent[1].project_path.ends_with("repo_a"));

        let open_file = store.get_last_open_file(&root_b);
        assert_eq!(open_file.as_deref(), Some("src/b.rs"));

        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn file_open_times_track_per_file_across_record_kinds() {
        let timestamp = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("gargo_file_opens_{}", timestamp));
        fs::create_dir_all(&temp_dir).unwrap();

        let root = temp_dir.join("repo");
        fs::create_dir_all(root.join("src")).unwrap();
        let file_a = root.join("src").join("a.rs");
        let file_b = root.join("README.md");
        fs::write(&file_a, "fn a() {}").unwrap();
        fs::write(&file_b, "# hi").unwrap();

        let store = RecentProjectsStore::new_with_data_dir(temp_dir.join("db"));
        store.record_file_open(&root, &file_a).unwrap();
        store.record_file_edit(&root, &file_b).unwrap();
        store.record_project_open(&root, Some(&file_a)).unwrap();

        let times = store.get_file_open_times(&root);
        assert_eq!(times.len(), 2);
        assert!(times.contains_key("src/a.rs"));
        assert!(times.contains_key("README.md"));
        // a.rs was touched twice (open then project-open); its time is the later one.
        assert!(times["src/a.rs"] >= times["README.md"]);

        fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn record_file_outside_project_is_ignored() {
        let timestamp = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir =
            std::env::temp_dir().join(format!("gargo_recent_projects_oob_{}", timestamp));
        fs::create_dir_all(&temp_dir).unwrap();

        let root = temp_dir.join("repo");
        let outside = temp_dir.join("outside.txt");
        fs::create_dir_all(&root).unwrap();
        fs::write(&outside, "x").unwrap();

        let store = RecentProjectsStore::new_with_data_dir(temp_dir.join("db"));
        store.record_file_open(&root, &outside).unwrap();
        store.record_file_edit(&root, &outside).unwrap();

        let recent = store.get_recent_projects(10);
        assert!(recent.is_empty());

        fs::remove_dir_all(&temp_dir).ok();
    }
}
