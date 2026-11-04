//! SQLite session history and a transactional spending ledger.
use crate::{
    accounting::Pricing,
    model::{Message, Usage},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Session {
    pub id: String,
    pub project: String,
    pub model: String,
    pub created: String,
}

#[derive(Debug, Serialize)]
pub struct LedgerEntry {
    pub id: String,
    pub session: String,
    pub project: String,
    pub day: String,
    pub state: String,
    pub reserved_nusd: Option<u64>,
    pub cost_nusd: Option<u64>,
    pub estimated_input: u64,
    pub usage: Option<Usage>,
    pub pricing: Option<Pricing>,
}

#[derive(Clone, Debug)]
pub struct Reservation {
    pub id: String,
    pub amount: Option<u64>,
    pub warning: bool,
}

impl Store {
    /// Prevent concurrent turns in one session; the OS releases this lock on crash.
    pub fn lock_session(&self, id: &str) -> Result<std::fs::File> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            bail!("invalid session identifier");
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_extension(format!("{id}.lock")))?;
        fs2::FileExt::try_lock_exclusive(&file)
            .context("session is already active in another Forge process")?;
        Ok(file)
    }
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let this = Self { path: path.into() };
        let conn = this.connect()?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            bail!("database was created by a newer Forge version");
        }
        conn.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,project TEXT NOT NULL,model TEXT NOT NULL,created TEXT NOT NULL,context TEXT NOT NULL DEFAULT '[]');
            CREATE TABLE IF NOT EXISTS history(seq INTEGER PRIMARY KEY,session TEXT NOT NULL,kind TEXT NOT NULL,payload TEXT NOT NULL,created TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
            CREATE TABLE IF NOT EXISTS requests(id TEXT PRIMARY KEY,session TEXT NOT NULL,project TEXT NOT NULL,day TEXT NOT NULL,state TEXT NOT NULL,reserved INTEGER,cost INTEGER,estimated_input INTEGER NOT NULL,usage TEXT,pricing TEXT);
            CREATE INDEX IF NOT EXISTS requests_scope ON requests(project,session,day);
            CREATE TABLE IF NOT EXISTS budgets(scope TEXT NOT NULL,target TEXT NOT NULL,amount INTEGER NOT NULL,PRIMARY KEY(scope,target));
            PRAGMA user_version=1;")?;
        Ok(this)
    }
    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(conn)
    }
    pub fn create_session(&self, project: &str, model: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.connect()?.execute(
            "INSERT INTO sessions(id,project,model,created) VALUES(?1,?2,?3,?4)",
            params![id, project, model, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(id)
    }
    pub fn sessions(&self) -> Result<Vec<Session>> {
        let c = self.connect()?;
        let mut s =
            c.prepare("SELECT id,project,model,created FROM sessions ORDER BY created DESC")?;
        Ok(s.query_map([], |r| {
            Ok(Session {
                id: r.get(0)?,
                project: r.get(1)?,
                model: r.get(2)?,
                created: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn session(&self, id: &str) -> Result<Session> {
        self.connect()?
            .query_row(
                "SELECT id,project,model,created FROM sessions WHERE id=?1",
                [id],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        project: r.get(1)?,
                        model: r.get(2)?,
                        created: r.get(3)?,
                    })
                },
            )
            .context("session not found")
    }
    pub fn context(&self, id: &str) -> Result<Vec<Message>> {
        let text: String =
            self.connect()?
                .query_row("SELECT context FROM sessions WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
        Ok(serde_json::from_str(&text)?)
    }
    pub fn append(&self, id: &str, message: &Message) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let text: String = tx.query_row("SELECT context FROM sessions WHERE id=?1", [id], |r| {
            r.get(0)
        })?;
        let mut context: Vec<Message> = serde_json::from_str(&text)?;
        context.push(message.clone());
        tx.execute(
            "UPDATE sessions SET context=?2 WHERE id=?1",
            params![id, serde_json::to_string(&context)?],
        )?;
        tx.execute(
            "INSERT INTO history(session,kind,payload) VALUES(?1,'message',?2)",
            params![id, serde_json::to_string(message)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn compact(&self, id: &str, context: &[Message]) -> Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let text = serde_json::to_string(context)?;
        tx.execute(
            "INSERT INTO history(session,kind,payload) VALUES(?1,'compaction',?2)",
            params![id, text],
        )?;
        tx.execute(
            "UPDATE sessions SET context=?2 WHERE id=?1",
            params![id, text],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn record(&self, id: &str, kind: &str, payload: &impl Serialize) -> Result<()> {
        self.connect()?.execute(
            "INSERT INTO history(session,kind,payload) VALUES(?1,?2,?3)",
            params![id, kind, serde_json::to_string(payload)?],
        )?;
        Ok(())
    }
    /// Read the latest 40 refinement records, newest first, for an existing session.
    pub fn refinements(&self, id: &str) -> Result<Vec<serde_json::Value>> {
        self.session(id)?;
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT payload FROM history WHERE session=?1 AND kind='refinement_stage' ORDER BY seq DESC LIMIT 40",
        )?;
        let rows = statement.query_map([id], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
    pub fn set_budget(&self, scope: &str, target: &str, amount: Option<u64>) -> Result<()> {
        if !matches!(scope, "session" | "project" | "daily") {
            bail!("invalid budget scope");
        }
        let c = self.connect()?;
        match amount {
            Some(n) => {
                c.execute("INSERT INTO budgets VALUES(?1,?2,?3) ON CONFLICT(scope,target) DO UPDATE SET amount=excluded.amount",params![scope,target,i64::try_from(n)?])?;
            }
            None => {
                c.execute(
                    "DELETE FROM budgets WHERE scope=?1 AND target=?2",
                    params![scope, target],
                )?;
            }
        }
        Ok(())
    }
    pub fn budgets(&self) -> Result<Vec<(String, String, u64)>> {
        let c = self.connect()?;
        let mut s = c.prepare("SELECT scope,target,amount FROM budgets ORDER BY scope,target")?;
        Ok(s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn reserve(
        &self,
        session: &str,
        project: &str,
        input: u64,
        amount: Option<u64>,
        pricing: Option<&Pricing>,
    ) -> Result<Reservation> {
        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut warning = false;
        for (scope, target, column, value) in [
            ("session", session, "session", session),
            ("project", project, "project", project),
            ("daily", "*", "day", day.as_str()),
        ] {
            let budget = tx.query_row(
                "SELECT amount FROM budgets WHERE scope=?1 AND target=?2",
                params![scope, target],
                |r| r.get::<_, u64>(0),
            );
            let limit = match budget {
                Ok(n) => n,
                Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                Err(e) => return Err(e.into()),
            };
            let amount =
                amount.context("unpriced model cannot be used under an active monetary budget")?;
            // Unknown charges with no reservation cannot be safely bounded.
            let unknown:u64=tx.query_row(&format!("SELECT count(*) FROM requests WHERE {column}=?1 AND cost IS NULL AND reserved IS NULL"),[value],|r|r.get(0))?;
            if unknown > 0 {
                bail!("{scope} budget paused: unresolved unpriced request");
            }
            let spent:u64=tx.query_row(&format!("SELECT COALESCE(SUM(CASE WHEN state='complete' THEN COALESCE(cost,reserved,0) ELSE COALESCE(reserved,0) END),0) FROM requests WHERE {column}=?1"),[value],|r|r.get(0))?;
            let projected = spent
                .checked_add(amount)
                .context("budget arithmetic overflow")?;
            if projected > limit {
                bail!("{scope} budget exceeded; increase the limit or inspect unresolved requests");
            }
            warning |= u128::from(projected) * 100 >= u128::from(limit) * 80;
        }
        let id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO requests(id,session,project,day,state,reserved,estimated_input,pricing) VALUES(?1,?2,?3,?4,'pending',?5,?6,?7)",params![id,session,project,day,amount.map(i64::try_from).transpose()?,i64::try_from(input)?,pricing.map(serde_json::to_string).transpose()?])?;
        tx.commit()?;
        Ok(Reservation {
            id,
            amount,
            warning,
        })
    }
    pub fn settle(
        &self,
        id: &str,
        usage: Option<&Usage>,
        pricing: Option<&Pricing>,
    ) -> Result<Option<u64>> {
        let cost = match (usage, pricing) {
            (Some(u), Some(p)) => p.cost(u).ok(),
            _ => None,
        };
        let state = if usage.is_some() && (pricing.is_none() || cost.is_some()) {
            "complete"
        } else {
            "uncertain"
        };
        self.connect()?.execute(
            "UPDATE requests SET state=?2,cost=?3,usage=?4 WHERE id=?1",
            params![
                id,
                state,
                cost.map(i64::try_from).transpose()?,
                usage.map(serde_json::to_string).transpose()?
            ],
        )?;
        Ok(cost)
    }
    pub fn uncertain(&self, id: &str) -> Result<()> {
        self.connect()?.execute(
            "UPDATE requests SET state='uncertain' WHERE id=?1 AND state='pending'",
            [id],
        )?;
        Ok(())
    }
    /// Explicit user reconciliation after checking provider billing; never automatic.
    pub fn resolve_charge(&self, id: &str, cost: u64) -> Result<()> {
        let mut conn = self.connect()?;
        let session: String = conn
            .query_row("SELECT session FROM requests WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .context("request not found")?;
        let _lock = self.lock_session(&session)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE requests SET state='complete',cost=?2 WHERE id=?1",
            params![id, i64::try_from(cost)?],
        )?;
        tx.execute(
            "INSERT INTO history(session,kind,payload) VALUES(?1,'manual_charge_resolution',?2)",
            params![
                session,
                serde_json::json!({"request":id,"cost_nusd":cost}).to_string()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn ledger(&self) -> Result<Vec<LedgerEntry>> {
        let c = self.connect()?;
        let mut s=c.prepare("SELECT id,session,project,day,state,reserved,cost,estimated_input,usage,pricing FROM requests ORDER BY rowid")?;
        let rows = s.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<u64>>(5)?,
                r.get::<_, Option<u64>>(6)?,
                r.get::<_, u64>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })?;
        rows.map(|r| {
            let (
                id,
                session,
                project,
                day,
                state,
                reserved_nusd,
                cost_nusd,
                estimated_input,
                usage,
                pricing,
            ) = r?;
            Ok(LedgerEntry {
                id,
                session,
                project,
                day,
                state,
                reserved_nusd,
                cost_nusd,
                estimated_input,
                usage: usage.map(|s| serde_json::from_str(&s)).transpose()?,
                pricing: pricing.map(|s| serde_json::from_str(&s)).transpose()?,
            })
        })
        .collect()
    }
}

/// Keep blocking SQLite operations off Tokio worker threads.
pub async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .context("storage task failed")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_locks_and_explicit_reconciliation() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("x.db")).unwrap();
        let id = s.create_session("p", "m").unwrap();
        let guard = s.lock_session(&id).unwrap();
        assert!(s.lock_session(&id).is_err());
        let r = s.reserve(&id, "p", 1, Some(80), None).unwrap();
        assert!(s.resolve_charge(&r.id, 20).is_err());
        drop(guard);
        s.uncertain(&r.id).unwrap();
        s.resolve_charge(&r.id, 20).unwrap();
        assert_eq!(s.ledger().unwrap()[0].cost_nusd, Some(20));
        assert!(s.lock_session(&id).is_ok());
    }
    #[test]
    fn old_unpriced_requests_block_new_budgets_until_resolved() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("x.db")).unwrap();
        let id = s.create_session("p", "m").unwrap();
        let r = s.reserve(&id, "p", 1, None, None).unwrap();
        s.settle(&r.id, Some(&Usage::default()), None).unwrap();
        s.set_budget("project", "p", Some(100)).unwrap();
        assert!(s.reserve(&id, "p", 1, Some(10), None).is_err());
        s.resolve_charge(&r.id, 30).unwrap();
        assert!(s.reserve(&id, "p", 1, Some(70), None).is_ok());
    }
    #[test]
    fn concurrent_reservations_and_uncertainty() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("test.db")).unwrap();
        let id = s.create_session("p", "m").unwrap();
        s.set_budget("project", "p", Some(100)).unwrap();
        std::thread::scope(|scope| {
            let a = scope.spawn(|| s.reserve(&id, "p", 1, Some(60), None));
            let b = scope.spawn(|| s.reserve(&id, "p", 1, Some(60), None));
            assert_ne!(a.join().unwrap().is_ok(), b.join().unwrap().is_ok());
        });
        let request = s.ledger().unwrap().remove(0);
        s.uncertain(&request.id).unwrap();
        assert!(s.reserve(&id, "p", 1, Some(50), None).is_err());
        assert!(s.reserve(&id, "p", 1, None, None).is_err());
    }
    #[test]
    fn history_survives_compaction() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::open(&d.path().join("x.db")).unwrap();
        let id = s.create_session("p", "m").unwrap();
        s.append(&id, &Message::text(crate::model::Role::User, "original"))
            .unwrap();
        s.compact(&id, &[Message::text(crate::model::Role::User, "summary")])
            .unwrap();
        assert_eq!(s.context(&id).unwrap()[0].text, "summary");
        let count: u64 = s
            .connect()
            .unwrap()
            .query_row("SELECT count(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }
}
