//! SQLite persistence for master data (users, cards, groups, schedules) and access history.

use std::path::Path;

use rusqlite::{Connection, params};

use crate::directory::{DenyReason, DirectoryError, UserDirectory};
use crate::model::{AccessGroup, CardId, GroupId, Schedule, User, UserId};
use crate::system::{AccessPoint, AccessRecord};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS schedules (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    weekly TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS access_groups (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    doors TEXT NOT NULL,
    key_cabinets TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    groups TEXT NOT NULL,
    valid_from TEXT NOT NULL,
    valid_until TEXT
);
CREATE TABLE IF NOT EXISTS cards (
    card BLOB PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS cards_user ON cards(user_id);
CREATE TABLE IF NOT EXISTS access_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at TEXT NOT NULL,
    point TEXT NOT NULL,
    card BLOB,
    user_id INTEGER,
    granted INTEGER NOT NULL,
    reason TEXT
);
";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Directory(#[from] DirectoryError),
    #[error("corrupt record: {0}")]
    Corrupt(String),
}

#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn save_schedule(&self, s: &Schedule) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO schedules (id, name, weekly) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, weekly = excluded.weekly",
            params![s.id.0, s.name, serde_json::to_string(&s.weekly)?],
        )?;
        Ok(())
    }

    pub fn save_group(&self, g: &AccessGroup) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO access_groups (id, name, doors, key_cabinets) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, doors = excluded.doors,
                 key_cabinets = excluded.key_cabinets",
            params![
                g.id.0,
                g.name,
                serde_json::to_string(&g.doors)?,
                serde_json::to_string(&g.key_cabinets)?
            ],
        )?;
        Ok(())
    }

    /// Inserts `s` only if no schedule with its id exists; saved schedules are kept.
    pub fn seed_schedule(&self, s: &Schedule) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO schedules (id, name, weekly) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO NOTHING",
            params![s.id.0, s.name, serde_json::to_string(&s.weekly)?],
        )?;
        Ok(())
    }

    /// Inserts `g` only if no group with its id exists; saved groups are kept.
    pub fn seed_group(&self, g: &AccessGroup) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO access_groups (id, name, doors, key_cabinets) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO NOTHING",
            params![
                g.id.0,
                g.name,
                serde_json::to_string(&g.doors)?,
                serde_json::to_string(&g.key_cabinets)?
            ],
        )?;
        Ok(())
    }

    /// Inserts or replaces users and their cards in a single transaction.
    pub fn save_users(&mut self, users: &[User]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO users (id, name, groups, valid_from, valid_until)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, groups = excluded.groups,
                     valid_from = excluded.valid_from, valid_until = excluded.valid_until",
            )?;
            let mut delete_cards = tx.prepare_cached("DELETE FROM cards WHERE user_id = ?1")?;
            let mut insert_card =
                tx.prepare_cached("INSERT INTO cards (card, user_id) VALUES (?1, ?2)")?;
            for u in users {
                upsert.execute(params![
                    u.id.0,
                    u.name,
                    join_ids(u.groups.iter().map(|g| g.0)),
                    u.valid_from,
                    u.valid_until
                ])?;
                delete_cards.execute([u.id.0])?;
                for card in &u.cards {
                    insert_card.execute(params![card.as_bytes(), u.id.0])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_user(&self, id: UserId) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .execute("DELETE FROM users WHERE id = ?1", [id.0])?
            > 0)
    }

    pub fn user_count(&self) -> Result<usize, StoreError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Loads all master data into an in-memory [`UserDirectory`].
    pub fn load_directory(&self) -> Result<UserDirectory, StoreError> {
        let mut dir = UserDirectory::with_capacity(self.user_count()?);
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, weekly FROM schedules")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            dir.insert_schedule(Schedule {
                id: crate::model::ScheduleId(r.get(0)?),
                name: r.get(1)?,
                weekly: serde_json::from_str(&r.get::<_, String>(2)?)?,
            });
        }
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, doors, key_cabinets FROM access_groups")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            dir.insert_group(AccessGroup {
                id: GroupId(r.get(0)?),
                name: r.get(1)?,
                doors: serde_json::from_str(&r.get::<_, String>(2)?)?,
                key_cabinets: serde_json::from_str(&r.get::<_, String>(3)?)?,
            })?;
        }

        let mut card_stmt = self
            .conn
            .prepare("SELECT user_id, card FROM cards ORDER BY user_id")?;
        let mut cards = card_stmt.query([])?;
        let mut next_card: Option<(u32, CardId)> = next_card_row(&mut cards)?;

        let mut stmt = self
            .conn
            .prepare("SELECT id, name, groups, valid_from, valid_until FROM users ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let id: u32 = r.get(0)?;
            let mut user_cards = Vec::new();
            while let Some((owner, card)) = next_card.take() {
                if owner == id {
                    user_cards.push(card);
                    next_card = next_card_row(&mut cards)?;
                } else {
                    next_card = Some((owner, card));
                    break;
                }
            }
            dir.upsert_user(User {
                id: UserId(id),
                name: r.get(1)?,
                cards: user_cards,
                groups: split_ids(&r.get::<_, String>(2)?)?
                    .into_iter()
                    .map(GroupId)
                    .collect(),
                valid_from: r.get(3)?,
                valid_until: r.get(4)?,
            })?;
        }
        Ok(dir)
    }

    pub fn append_access(&self, rec: &AccessRecord) -> Result<(), StoreError> {
        self.conn
            .prepare_cached(
                "INSERT INTO access_log (at, point, card, user_id, granted, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![
                rec.at,
                serde_json::to_string(&rec.point)?,
                rec.card.as_ref().map(CardId::as_bytes),
                rec.user.map(|u| u.0),
                rec.granted,
                rec.reason.as_ref().map(serde_json::to_string).transpose()?,
            ])?;
        Ok(())
    }

    /// Most recent access records first.
    pub fn recent_access(&self, limit: usize) -> Result<Vec<AccessRecord>, StoreError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT at, point, card, user_id, granted, reason FROM access_log
             ORDER BY id DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query([i64::try_from(limit).unwrap_or(i64::MAX)])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let point: AccessPoint = serde_json::from_str(&r.get::<_, String>(1)?)?;
            let card = r
                .get::<_, Option<Vec<u8>>>(2)?
                .map(|b| CardId::from_bytes(&b))
                .transpose()
                .map_err(|e| StoreError::Corrupt(e.to_string()))?;
            let reason: Option<DenyReason> = r
                .get::<_, Option<String>>(5)?
                .map(|s| serde_json::from_str(&s))
                .transpose()?;
            out.push(AccessRecord {
                at: r.get(0)?,
                point,
                card,
                user: r.get::<_, Option<u32>>(3)?.map(UserId),
                granted: r.get(4)?,
                reason,
            });
        }
        Ok(out)
    }
}

fn next_card_row(rows: &mut rusqlite::Rows<'_>) -> Result<Option<(u32, CardId)>, StoreError> {
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    let bytes: Vec<u8> = r.get(1)?;
    let card = CardId::from_bytes(&bytes).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(Some((r.get(0)?, card)))
}

fn join_ids(ids: impl Iterator<Item = u32>) -> String {
    ids.map(|i| i.to_string()).collect::<Vec<_>>().join(",")
}

fn split_ids(s: &str) -> Result<Vec<u32>, StoreError> {
    s.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.parse()
                .map_err(|_| StoreError::Corrupt(format!("group list {s:?}")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    use crate::model::DoorId;

    #[test]
    fn seeding_keeps_saved_master_data() {
        let store = Store::open_in_memory().unwrap();
        let site = demo::site(1);
        let mut group = site.directory.groups().next().unwrap().clone();
        group.doors.clear();
        store.save_group(&group).unwrap();
        for g in site.directory.groups() {
            store.seed_group(g).unwrap();
        }
        let loaded = store.load_directory().unwrap();
        let kept = loaded.groups().find(|g| g.id == group.id).unwrap();
        assert!(kept.doors.is_empty());
        assert_eq!(loaded.groups().count(), site.directory.groups().count());
    }

    #[test]
    fn master_data_round_trips() {
        let site = demo::site(100);
        let mut store = Store::open_in_memory().unwrap();
        demo::save(&site.directory, &mut store).unwrap();
        let loaded = store.load_directory().unwrap();
        assert_eq!(loaded.len(), 100);
        let u = site.directory.user(UserId(42)).unwrap();
        assert_eq!(loaded.user(UserId(42)), Some(u));
        assert_eq!(
            loaded.user_by_card(&u.cards[0]).map(|u| u.id),
            Some(UserId(42))
        );

        let mut changed = u.clone();
        changed.cards = vec!["ABCD".parse().unwrap()];
        store.save_users(&[changed]).unwrap();
        assert!(store.delete_user(UserId(1)).unwrap());
        let loaded = store.load_directory().unwrap();
        assert_eq!(loaded.len(), 99);
        assert!(loaded.user_by_card(&u.cards[0]).is_none());
        assert_eq!(
            loaded.user_by_card(&"ABCD".parse().unwrap()).map(|u| u.id),
            Some(UserId(42))
        );
    }

    #[test]
    fn access_log_round_trips_newest_first() {
        let store = Store::open_in_memory().unwrap();
        let at = demo::epoch();
        for (i, granted) in [true, false].into_iter().enumerate() {
            store
                .append_access(&AccessRecord {
                    at: at + chrono::TimeDelta::seconds(i as i64),
                    point: AccessPoint::Door(DoorId(1)),
                    card: Some(demo::card(1)),
                    user: Some(UserId(1)),
                    granted,
                    reason: (!granted).then_some(DenyReason::NoPermission),
                })
                .unwrap();
        }
        let recent = store.recent_access(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert!(!recent[0].granted);
        assert_eq!(recent[0].reason, Some(DenyReason::NoPermission));
        assert_eq!(recent[1].card, Some(demo::card(1)));
    }
}
