//! SQLite storage. One connection behind a mutex: kall serves a handful of people, and every
//! query here is a few microseconds.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use kall_proto::{Channel, ChannelKind, Id, Invite, Message, Millis, Preview, ReadState, Role, Status, User};
use rusqlite::{Connection, OptionalExtension, params};

pub struct Db(Mutex<Connection>);

pub fn now() -> Millis {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    pass_hash TEXT NOT NULL,
    role INTEGER NOT NULL DEFAULT 0,
    status INTEGER NOT NULL DEFAULT 0,
    status_text TEXT NOT NULL DEFAULT '',
    avatar BLOB,
    avatar_rev INTEGER NOT NULL DEFAULT 0,
    banned INTEGER NOT NULL DEFAULT 0,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    user INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS invites (
    code TEXT PRIMARY KEY,
    created_by INTEGER,
    uses_left INTEGER,
    expires INTEGER,
    grants_owner INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS channels (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    kind INTEGER NOT NULL,
    position INTEGER NOT NULL,
    bitrate INTEGER NOT NULL DEFAULT 64000
);
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    channel INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    author INTEGER NOT NULL,
    body TEXT NOT NULL,
    reply_to INTEGER,
    created INTEGER NOT NULL,
    edited INTEGER,
    deleted INTEGER NOT NULL DEFAULT 0,
    preview BLOB
);
CREATE INDEX IF NOT EXISTS messages_by_channel ON messages(channel, id);
CREATE TABLE IF NOT EXISTS read_state (
    user INTEGER NOT NULL,
    channel INTEGER NOT NULL,
    last_read INTEGER NOT NULL,
    PRIMARY KEY (user, channel)
);
";

fn kind_to_i64(k: ChannelKind) -> i64 {
    match k {
        ChannelKind::Text => 0,
        ChannelKind::Voice => 1,
    }
}

fn kind_from_i64(v: i64) -> ChannelKind {
    if v == 1 { ChannelKind::Voice } else { ChannelKind::Text }
}

pub struct Account {
    pub id: Id,
    pub pass_hash: String,
    pub banned: bool,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        let db = Db(Mutex::new(conn));
        db.seed()?;
        Ok(db)
    }

    pub fn memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        let db = Db(Mutex::new(conn));
        db.seed()?;
        Ok(db)
    }

    fn c(&self) -> MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A fresh server gets a couple of channels to start with.
    fn seed(&self) -> Result<()> {
        let c = self.c();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))?;
        if n == 0 {
            c.execute("INSERT INTO channels (name, kind, position) VALUES ('general', 0, 0), ('Lounge', 1, 1)", [])?;
        }
        Ok(())
    }

    // ---- meta ----

    pub fn user_count(&self) -> Result<i64> {
        Ok(self.c().query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
    }

    // ---- invites ----

    pub fn create_invite(&self, code: &str, by: Option<Id>, uses: Option<u32>, expires: Option<Millis>, owner: bool) -> Result<Invite> {
        self.c().execute(
            "INSERT INTO invites (code, created_by, uses_left, expires, grants_owner) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![code, by, uses, expires, owner as i64],
        )?;
        Ok(Invite { code: code.into(), uses_left: uses, expires })
    }

    pub fn invites(&self) -> Result<Vec<Invite>> {
        let c = self.c();
        let mut st = c.prepare("SELECT code, uses_left, expires FROM invites WHERE grants_owner = 0 ORDER BY rowid")?;
        let rows = st.query_map([], |r| Ok(Invite { code: r.get(0)?, uses_left: r.get(1)?, expires: r.get(2)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn has_owner_invite(&self) -> Result<Option<String>> {
        Ok(self.c().query_row("SELECT code FROM invites WHERE grants_owner = 1", [], |r| r.get(0)).optional()?)
    }

    pub fn revoke_invite(&self, code: &str) -> Result<()> {
        self.c().execute("DELETE FROM invites WHERE code = ?1", [code])?;
        Ok(())
    }

    /// Spend one use of an invite and create the account in one transaction.
    pub fn register(&self, invite: &str, username: &str, pass_hash: &str) -> Result<Id> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let row: Option<(Option<i64>, Option<i64>, bool)> = tx
            .query_row("SELECT uses_left, expires, grants_owner FROM invites WHERE code = ?1", [invite], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0))
            })
            .optional()?;
        let Some((uses_left, expires, owner)) = row else { bail!("That invite code doesn't exist.") };
        if expires.is_some_and(|e| e < now()) {
            bail!("That invite has expired.");
        }
        let taken: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE username = ?1)", [username], |r| r.get(0))?;
        if taken {
            bail!("That username is taken.");
        }
        match uses_left {
            Some(n) if n <= 1 => {
                tx.execute("DELETE FROM invites WHERE code = ?1", [invite])?;
            }
            Some(_) => {
                tx.execute("UPDATE invites SET uses_left = uses_left - 1 WHERE code = ?1", [invite])?;
            }
            None => {}
        }
        let role = if owner { Role::Owner } else { Role::Member } as i64;
        tx.execute(
            "INSERT INTO users (username, display_name, pass_hash, role, created) VALUES (?1, ?1, ?2, ?3, ?4)",
            params![username, pass_hash, role, now()],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    // ---- accounts & sessions ----

    pub fn account(&self, username: &str) -> Result<Option<Account>> {
        Ok(self
            .c()
            .query_row("SELECT id, pass_hash, banned FROM users WHERE username = ?1", [username], |r| {
                Ok(Account { id: r.get(0)?, pass_hash: r.get(1)?, banned: r.get::<_, i64>(2)? != 0 })
            })
            .optional()?)
    }

    pub fn add_session(&self, token_hash: &str, user: Id) -> Result<()> {
        self.c().execute("INSERT INTO sessions (token_hash, user, created) VALUES (?1, ?2, ?3)", params![token_hash, user, now()])?;
        Ok(())
    }

    pub fn session_user(&self, token_hash: &str) -> Result<Option<Id>> {
        Ok(self
            .c()
            .query_row("SELECT s.user FROM sessions s JOIN users u ON u.id = s.user WHERE s.token_hash = ?1 AND u.banned = 0", [token_hash], |r| r.get(0))
            .optional()?)
    }

    pub fn drop_session(&self, token_hash: &str) -> Result<()> {
        self.c().execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])?;
        Ok(())
    }

    pub fn set_banned(&self, user: Id, banned: bool) -> Result<()> {
        let c = self.c();
        c.execute("UPDATE users SET banned = ?2 WHERE id = ?1", params![user, banned as i64])?;
        if banned {
            c.execute("DELETE FROM sessions WHERE user = ?1", [user])?;
        }
        Ok(())
    }

    // ---- users ----

    fn user_row(r: &rusqlite::Row) -> rusqlite::Result<User> {
        Ok(User {
            id: r.get(0)?,
            username: r.get(1)?,
            display_name: r.get(2)?,
            role: Role::from_i64(r.get(3)?),
            status: Status::from_i64(r.get(4)?),
            status_text: r.get(5)?,
            online: false,
            avatar_rev: r.get::<_, i64>(6)? as u32,
        })
    }

    const USER_COLS: &str = "id, username, display_name, role, status, status_text, avatar_rev";

    pub fn user(&self, id: Id) -> Result<Option<User>> {
        let sql = format!("SELECT {} FROM users WHERE id = ?1", Self::USER_COLS);
        Ok(self.c().query_row(&sql, [id], Self::user_row).optional()?)
    }

    pub fn users(&self) -> Result<Vec<User>> {
        let c = self.c();
        let mut st = c.prepare(&format!("SELECT {} FROM users WHERE banned = 0 ORDER BY id", Self::USER_COLS))?;
        let rows = st.query_map([], Self::user_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_profile(&self, id: Id, display_name: &str, status: Status, status_text: &str) -> Result<()> {
        self.c().execute(
            "UPDATE users SET display_name = ?2, status = ?3, status_text = ?4 WHERE id = ?1",
            params![id, display_name, status.to_i64(), status_text],
        )?;
        Ok(())
    }

    pub fn set_role(&self, id: Id, role: Role) -> Result<()> {
        self.c().execute("UPDATE users SET role = ?2 WHERE id = ?1", params![id, role as i64])?;
        Ok(())
    }

    pub fn set_avatar(&self, id: Id, png: &[u8]) -> Result<()> {
        self.c().execute("UPDATE users SET avatar = ?2, avatar_rev = avatar_rev + 1 WHERE id = ?1", params![id, png])?;
        Ok(())
    }

    pub fn avatar(&self, id: Id) -> Result<Option<(u32, Vec<u8>)>> {
        Ok(self
            .c()
            .query_row("SELECT avatar_rev, avatar FROM users WHERE id = ?1 AND avatar IS NOT NULL", [id], |r| {
                Ok((r.get::<_, i64>(0)? as u32, r.get(1)?))
            })
            .optional()?)
    }

    // ---- channels ----

    fn channel_row(r: &rusqlite::Row) -> rusqlite::Result<Channel> {
        Ok(Channel {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: kind_from_i64(r.get(2)?),
            position: r.get(3)?,
            bitrate: r.get::<_, i64>(4)? as u32,
            last_message: r.get(5)?,
        })
    }

    const CHANNEL_SQL: &str = "SELECT c.id, c.name, c.kind, c.position, c.bitrate,
        COALESCE((SELECT MAX(id) FROM messages m WHERE m.channel = c.id AND m.deleted = 0), 0) FROM channels c";

    pub fn channels(&self) -> Result<Vec<Channel>> {
        let c = self.c();
        let mut st = c.prepare(&format!("{} ORDER BY c.position, c.id", Self::CHANNEL_SQL))?;
        let rows = st.query_map([], Self::channel_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn channel(&self, id: Id) -> Result<Option<Channel>> {
        Ok(self.c().query_row(&format!("{} WHERE c.id = ?1", Self::CHANNEL_SQL), [id], Self::channel_row).optional()?)
    }

    pub fn create_channel(&self, name: &str, kind: ChannelKind) -> Result<Id> {
        let c = self.c();
        let pos: i64 = c.query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM channels", [], |r| r.get(0))?;
        c.execute("INSERT INTO channels (name, kind, position) VALUES (?1, ?2, ?3)", params![name, kind_to_i64(kind), pos])?;
        Ok(c.last_insert_rowid())
    }

    pub fn update_channel(&self, id: Id, name: &str, bitrate: u32) -> Result<()> {
        self.c().execute("UPDATE channels SET name = ?2, bitrate = ?3 WHERE id = ?1", params![id, name, bitrate])?;
        Ok(())
    }

    /// Swap a channel with its neighbour; returns the new order.
    pub fn move_channel(&self, id: Id, up: bool) -> Result<Vec<Id>> {
        let mut c = self.c();
        let tx = c.transaction()?;
        let mut rows: Vec<(Id, i64)> = {
            let mut st = tx.prepare("SELECT id, kind FROM channels ORDER BY position, id")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        // Swap with the nearest channel of the same kind in that direction.
        if let Some(i) = rows.iter().position(|&(x, _)| x == id) {
            let kind = rows[i].1;
            let j = if up { (0..i).rev().find(|&j| rows[j].1 == kind) } else { (i + 1..rows.len()).find(|&j| rows[j].1 == kind) };
            if let Some(j) = j {
                rows.swap(i, j);
            }
        }
        let ids: Vec<Id> = rows.into_iter().map(|(x, _)| x).collect();
        for (pos, cid) in ids.iter().enumerate() {
            tx.execute("UPDATE channels SET position = ?2 WHERE id = ?1", params![cid, pos as i64])?;
        }
        tx.commit()?;
        Ok(ids)
    }

    pub fn delete_channel(&self, id: Id) -> Result<()> {
        let c = self.c();
        c.execute("DELETE FROM messages WHERE channel = ?1", [id])?;
        c.execute("DELETE FROM read_state WHERE channel = ?1", [id])?;
        c.execute("DELETE FROM channels WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- messages ----

    fn message_row(r: &rusqlite::Row) -> rusqlite::Result<Message> {
        let preview: Option<Vec<u8>> = r.get(7)?;
        Ok(Message {
            id: r.get(0)?,
            channel: r.get(1)?,
            author: r.get(2)?,
            body: r.get(3)?,
            reply_to: r.get(4)?,
            created: r.get(5)?,
            edited: r.get(6)?,
            preview: preview.and_then(|b| postcard::from_bytes::<Preview>(&b).ok()),
        })
    }

    const MSG_COLS: &str = "id, channel, author, body, reply_to, created, edited, preview";

    pub fn insert_message(&self, channel: Id, author: Id, body: &str, reply_to: Option<Id>) -> Result<Message> {
        let created = now();
        let c = self.c();
        c.execute(
            "INSERT INTO messages (channel, author, body, reply_to, created) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![channel, author, body, reply_to, created],
        )?;
        Ok(Message { id: c.last_insert_rowid(), channel, author, body: body.into(), reply_to, created, edited: None, preview: None })
    }

    pub fn message(&self, id: Id) -> Result<Option<Message>> {
        let sql = format!("SELECT {} FROM messages WHERE id = ?1 AND deleted = 0", Self::MSG_COLS);
        Ok(self.c().query_row(&sql, [id], Self::message_row).optional()?)
    }

    pub fn edit_message(&self, id: Id, body: &str) -> Result<()> {
        self.c().execute("UPDATE messages SET body = ?2, edited = ?3, preview = NULL WHERE id = ?1", params![id, body, now()])?;
        Ok(())
    }

    pub fn set_preview(&self, id: Id, preview: &Preview) -> Result<()> {
        self.c().execute("UPDATE messages SET preview = ?2 WHERE id = ?1", params![id, postcard::to_stdvec(preview)?])?;
        Ok(())
    }

    /// Soft delete: replies keep pointing at an id that now reads as deleted.
    pub fn delete_message(&self, id: Id) -> Result<()> {
        self.c().execute("UPDATE messages SET deleted = 1, body = '', preview = NULL WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn history(&self, channel: Id, before: Option<Id>, limit: u32) -> Result<(Vec<Message>, bool)> {
        let limit = limit.clamp(1, 200);
        let c = self.c();
        let sql = format!(
            "SELECT {} FROM messages WHERE channel = ?1 AND deleted = 0 AND id < ?2 ORDER BY id DESC LIMIT ?3",
            Self::MSG_COLS
        );
        let mut st = c.prepare(&sql)?;
        let mut msgs: Vec<Message> = st
            .query_map(params![channel, before.unwrap_or(i64::MAX), limit + 1], Self::message_row)?
            .collect::<Result<_, _>>()?;
        let complete = msgs.len() <= limit as usize;
        msgs.truncate(limit as usize);
        msgs.reverse();
        Ok((msgs, complete))
    }

    // ---- read state ----

    pub fn mark_read(&self, user: Id, channel: Id, message: Id) -> Result<()> {
        self.c().execute(
            "INSERT INTO read_state (user, channel, last_read) VALUES (?1, ?2, ?3)
             ON CONFLICT(user, channel) DO UPDATE SET last_read = MAX(last_read, excluded.last_read)",
            params![user, channel, message],
        )?;
        Ok(())
    }

    pub fn read_state(&self, user: Id, username: &str, channel: Id) -> Result<ReadState> {
        let c = self.c();
        let last_read: Id = c
            .query_row("SELECT last_read FROM read_state WHERE user = ?1 AND channel = ?2", params![user, channel], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        // A cheap over-approximation, then exact parsing of the few candidates.
        let mut st = c.prepare(
            "SELECT body FROM messages WHERE channel = ?1 AND id > ?2 AND deleted = 0 AND author != ?3 AND body LIKE '%@' || ?4 || '%'",
        )?;
        let bodies: Vec<String> = st.query_map(params![channel, last_read, user, username], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mentions = bodies.iter().filter(|b| kall_proto::mentions(b).iter().any(|m| m == username)).count() as u32;
        Ok(ReadState { channel, last_read, mentions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_registration_and_history() -> Result<()> {
        let db = Db::memory()?;
        db.create_invite("own", None, Some(1), None, true)?;
        let me = db.register("own", "sam", "h")?;
        assert_eq!(db.user(me)?.unwrap().role, Role::Owner);
        assert!(db.register("own", "other", "h").is_err(), "single-use invite is spent");

        let ch = db.channels()?[0].id;
        for i in 0..5 {
            db.insert_message(ch, me, &format!("m{i}"), None)?;
        }
        let (page, complete) = db.history(ch, None, 3)?;
        assert_eq!(page.iter().map(|m| m.body.as_str()).collect::<Vec<_>>(), ["m2", "m3", "m4"]);
        assert!(!complete);
        let (older, complete) = db.history(ch, Some(page[0].id), 3)?;
        assert_eq!(older.len(), 2);
        assert!(complete);
        Ok(())
    }

    #[test]
    fn mention_counts() -> Result<()> {
        let db = Db::memory()?;
        db.create_invite("x", None, None, None, false)?;
        let a = db.register("x", "ana", "h")?;
        let b = db.register("x", "bob", "h")?;
        let ch = db.channels()?[0].id;
        db.insert_message(ch, a, "hi @bob", None)?;
        db.insert_message(ch, a, "@bobby isn't bob", None)?;
        let m = db.insert_message(ch, a, "hello", None)?;
        assert_eq!(db.read_state(b, "bob", ch)?.mentions, 1);
        db.mark_read(b, ch, m.id)?;
        assert_eq!(db.read_state(b, "bob", ch)?.mentions, 0);
        Ok(())
    }
}
