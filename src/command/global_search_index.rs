use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use regex::RegexBuilder;
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA_VERSION: i32 = 1;

#[derive(Debug, Clone)]
pub struct GlobalIndexedRepo {
    pub root: PathBuf,
    pub display_name: String,
}

#[derive(Debug, Clone)]
pub struct GlobalSearchHit {
    pub repo_root: PathBuf,
    pub display_path: String,
    pub rel_path: String,
    pub line: usize,
    pub char_col: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStat {
    size: i64,
    mtime: i64,
}

pub fn data_dir() -> PathBuf {
    if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg_data).join("gargo")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/gargo")
    } else {
        PathBuf::from(".gargo")
    }
}

pub fn discover_recent_git_repos(limit: usize) -> Vec<GlobalIndexedRepo> {
    let store = crate::command::recent_projects::RecentProjectsStore::new();
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    for entry in store.get_recent_projects(limit) {
        let path = PathBuf::from(entry.project_path);
        if !path.is_dir() {
            continue;
        }
        let Ok(root) = crate::command::git::repo_root_for_path(&path) else {
            continue;
        };
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        if !seen.insert(root.clone()) {
            continue;
        }
        roots.push(root);
    }

    let display_names = disambiguate_repo_names(&roots);
    roots
        .into_iter()
        .zip(display_names)
        .map(|(root, display_name)| GlobalIndexedRepo { root, display_name })
        .collect()
}

pub fn search_repo(
    repo: &GlobalIndexedRepo,
    query: &str,
    max_results: usize,
) -> Vec<GlobalSearchHit> {
    search_repo_limited(repo, query, max_results, usize::MAX)
}

/// Like [`search_repo`] but also caps matches *per file* at `per_file_max`, so a
/// single match-heavy file can't exhaust the global `max_results` budget and
/// crowd out other matching files. Files are still visited in path order; once a
/// file reaches `per_file_max` we move on to the next, letting results span far
/// more files for common terms. Pass `usize::MAX` for no per-file limit.
pub fn search_repo_limited(
    repo: &GlobalIndexedRepo,
    query: &str,
    max_results: usize,
    per_file_max: usize,
) -> Vec<GlobalSearchHit> {
    if query.chars().count() < 3 || max_results == 0 || per_file_max == 0 {
        return Vec::new();
    }

    let Ok(mut index) = RepoIndex::open(&repo.root) else {
        return Vec::new();
    };
    if index.refresh().is_err() {
        return Vec::new();
    }

    let Ok(candidates) = index.lookup(query) else {
        return Vec::new();
    };
    let regex = match RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
    {
        Ok(regex) => regex,
        Err(_) => return Vec::new(),
    };

    let mut hits = Vec::new();
    for rel_path in candidates {
        let full_path = repo.root.join(&rel_path);
        let Ok(content) = std::fs::read_to_string(&full_path) else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();
        let mut file_hits = 0;
        for (line_idx, line) in lines.iter().enumerate() {
            let Some(m) = regex.find(line) else {
                continue;
            };
            let char_col = line[..m.start()].chars().count();
            hits.push(GlobalSearchHit {
                repo_root: repo.root.clone(),
                display_path: format!("{}/{}", repo.display_name, rel_path),
                rel_path: rel_path.clone(),
                line: line_idx,
                char_col,
                excerpt: line.trim_end().to_string(),
            });
            if hits.len() >= max_results {
                return hits;
            }
            file_hits += 1;
            if file_hits >= per_file_max {
                break;
            }
        }
    }

    hits
}

struct RepoIndex {
    root: PathBuf,
    conn: Connection,
}

impl RepoIndex {
    fn open(root: &Path) -> rusqlite::Result<Self> {
        let dir = data_dir().join("global_search");
        let _ = std::fs::create_dir_all(&dir);
        let db_path = dir.join(format!("{}.sqlite3", stable_repo_key(root)));
        let conn = Connection::open(db_path)?;
        let index = Self {
            root: root.to_path_buf(),
            conn,
        };
        index.init_schema()?;
        Ok(index)
    }

    fn init_schema(&self) -> rusqlite::Result<()> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);
        if version != 0 && version != SCHEMA_VERSION {
            self.conn.execute_batch(
                "DROP TABLE IF EXISTS postings;
                 DROP TABLE IF EXISTS files;",
            )?;
        }

        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS files (
                file_id INTEGER PRIMARY KEY,
                rel_path TEXT NOT NULL UNIQUE,
                size INTEGER NOT NULL,
                mtime INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS postings (
                trigram BLOB NOT NULL,
                file_id INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_postings_trigram ON postings(trigram, file_id);
            CREATE INDEX IF NOT EXISTS idx_files_rel_path ON files(rel_path);
            PRAGMA user_version = 1;",
        )
    }

    fn refresh(&mut self) -> rusqlite::Result<()> {
        let current_files = collect_repo_files(&self.root);
        let current_set: HashSet<&str> = current_files.iter().map(String::as_str).collect();

        let mut known_stmt = self
            .conn
            .prepare("SELECT file_id, rel_path, size, mtime FROM files")?;
        let known_rows = known_stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                FileStat {
                    size: row.get(2)?,
                    mtime: row.get(3)?,
                },
            ))
        })?;

        let mut known = Vec::new();
        for row in known_rows {
            known.push(row?);
        }
        drop(known_stmt);

        let tx = self.conn.transaction()?;
        for (file_id, rel_path, _) in &known {
            if !current_set.contains(rel_path.as_str()) {
                tx.execute("DELETE FROM postings WHERE file_id = ?1", params![file_id])?;
                tx.execute("DELETE FROM files WHERE file_id = ?1", params![file_id])?;
            }
        }

        for rel_path in current_files {
            let Some(stat) = stat_file(&self.root.join(&rel_path)) else {
                continue;
            };
            let existing = known
                .iter()
                .find(|(_, known_path, _)| known_path == &rel_path)
                .map(|(file_id, _, known_stat)| (*file_id, *known_stat));
            if existing.is_some_and(|(_, known_stat)| known_stat == stat) {
                continue;
            }

            if let Some((file_id, _)) = existing {
                tx.execute("DELETE FROM postings WHERE file_id = ?1", params![file_id])?;
                tx.execute(
                    "UPDATE files SET size = ?2, mtime = ?3 WHERE file_id = ?1",
                    params![file_id, stat.size, stat.mtime],
                )?;
                insert_postings(&tx, file_id, &self.root.join(&rel_path))?;
            } else {
                tx.execute(
                    "INSERT INTO files (rel_path, size, mtime) VALUES (?1, ?2, ?3)",
                    params![rel_path, stat.size, stat.mtime],
                )?;
                let file_id = tx.last_insert_rowid();
                insert_postings(&tx, file_id, &self.root.join(&rel_path))?;
            }
        }

        tx.commit()
    }

    fn lookup(&self, query: &str) -> rusqlite::Result<Vec<String>> {
        let trigrams = trigrams_for_bytes(&ascii_lower_bytes(query.as_bytes()));
        if trigrams.is_empty() {
            return Ok(Vec::new());
        }

        let mut file_sets: Vec<BTreeSet<i64>> = Vec::new();
        for trigram in trigrams {
            let mut stmt = self
                .conn
                .prepare("SELECT file_id FROM postings WHERE trigram = ?1")?;
            let rows = stmt.query_map(params![trigram.as_slice()], |row| row.get::<_, i64>(0))?;
            let mut set = BTreeSet::new();
            for row in rows {
                set.insert(row?);
            }
            if set.is_empty() {
                return Ok(Vec::new());
            }
            file_sets.push(set);
        }

        file_sets.sort_by_key(|set| set.len());
        let mut iter = file_sets.into_iter();
        let mut intersection = iter.next().unwrap_or_default();
        for set in iter {
            intersection = intersection.intersection(&set).copied().collect();
            if intersection.is_empty() {
                return Ok(Vec::new());
            }
        }

        let mut paths = Vec::new();
        for file_id in intersection {
            if let Some(path) = self
                .conn
                .query_row(
                    "SELECT rel_path FROM files WHERE file_id = ?1",
                    params![file_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }
}

fn insert_postings(conn: &Connection, file_id: i64, path: &Path) -> rusqlite::Result<()> {
    let Ok(content) = std::fs::read(path) else {
        return Ok(());
    };
    if content.contains(&0) {
        return Ok(());
    }
    let lowered = ascii_lower_bytes(&content);
    for trigram in trigrams_for_bytes(&lowered) {
        conn.execute(
            "INSERT INTO postings (trigram, file_id) VALUES (?1, ?2)",
            params![trigram.as_slice(), file_id],
        )?;
    }
    Ok(())
}

fn collect_repo_files(root: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.quotepath=off",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.replace('\\', "/"))
        .collect();
    files.sort();
    files.dedup();
    files
}

fn stat_file(path: &Path) -> Option<FileStat> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let size = metadata.len() as i64;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    Some(FileStat { size, mtime })
}

fn ascii_lower_bytes(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|byte| byte.to_ascii_lowercase()).collect()
}

fn trigrams_for_bytes(bytes: &[u8]) -> BTreeSet<[u8; 3]> {
    let mut trigrams = BTreeSet::new();
    if bytes.len() < 3 {
        return trigrams;
    }
    for window in bytes.windows(3) {
        trigrams.insert([window[0], window[1], window[2]]);
    }
    trigrams
}

fn stable_repo_key(root: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.to_string_lossy().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn disambiguate_repo_names(roots: &[PathBuf]) -> Vec<String> {
    let basenames: Vec<String> = roots
        .iter()
        .map(|root| {
            root.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| root.display().to_string())
        })
        .collect();

    basenames
        .iter()
        .enumerate()
        .map(|(idx, base)| {
            if basenames.iter().filter(|name| *name == base).count() <= 1 {
                return base.clone();
            }
            let parent = roots[idx]
                .parent()
                .and_then(|p| p.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| roots[idx].display().to_string());
            format!("{parent}/{base}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigrams_are_ascii_case_folded_and_deduped() {
        let lowered = ascii_lower_bytes(b"AbCabc");
        let trigrams = trigrams_for_bytes(&lowered);
        assert!(trigrams.contains(b"abc"));
        assert!(trigrams.contains(b"bca"));
        assert_eq!(trigrams.len(), 3);
    }

    #[test]
    fn duplicate_repo_names_include_parent_suffix() {
        let roots = vec![
            PathBuf::from("/tmp/work/gargo"),
            PathBuf::from("/tmp/home/gargo"),
        ];
        let names = disambiguate_repo_names(&roots);
        assert_eq!(names, vec!["work/gargo", "home/gargo"]);
    }
}
