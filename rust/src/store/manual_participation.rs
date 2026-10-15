use super::*;
use rusqlite::OptionalExtension;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS manual_participation(
    id INTEGER PRIMARY KEY CHECK(id=1), mode TEXT NOT NULL);";

impl Store {
    pub(super) fn manual_participation_table_exists(db: &Connection) -> Result<bool> {
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='manual_participation')",
            [], |row| row.get(0),
        )?)
    }

    pub(super) fn has_existing_state(db: &Connection) -> Result<bool> {
        Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages')",
            [], |row| row.get(0),
        )?)
    }

    pub(crate) fn manual_participation_migration_required_on(db: &Connection) -> Result<bool> {
        Ok(Self::has_existing_state(db)? && !Self::manual_participation_table_exists(db)?)
    }

    pub fn requires_manual_participation_migration(path: impl AsRef<Path>) -> Result<bool> {
        let db = Connection::open(path)?;
        Self::manual_participation_migration_required_on(&db)
    }

    pub fn migrate_manual_participation(path: impl AsRef<Path>) -> Result<()> {
        let mut db = Connection::open(path)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let new_table = !Self::manual_participation_table_exists(&tx)?;
        tx.execute_batch(SCHEMA)?;
        if new_table {
            tx.execute("INSERT INTO manual_participation(id,mode) VALUES(1,'normal')", [])?;
        }
        Self::read_manual_participation_on(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn read_manual_participation_on(db: &Connection) -> Result<String> {
        let mode: String = db.query_row(
            "SELECT mode FROM manual_participation WHERE id=1", [], |row| row.get(0),
        ).optional()?.ok_or_else(|| anyhow::anyhow!("manual_participation_missing_row"))?;
        ensure!(matches!(mode.as_str(), "normal" | "paused" | "stopped"),
            "manual_participation_invalid_mode: {mode}");
        Ok(mode)
    }

    pub fn manual_participation_mode(&self) -> Result<&str> {
        let mode = Self::read_manual_participation_on(&self.db)?;
        Ok(match mode.as_str() {
            "normal" => "normal",
            "paused" => "paused",
            "stopped" => "stopped",
            _ => unreachable!(),
        })
    }

    pub fn take_manual_participation_initialized(&self) -> bool {
        self.manual_participation_initialized.replace(false)
    }

    pub fn save_manual_participation(&self, mode: &str) -> Result<()> {
        ensure!(matches!(mode, "normal" | "paused" | "stopped"), "manual_participation_invalid_mode");
        let tx = self.immediate()?;
        let changed = self.db.execute(
            "UPDATE manual_participation SET mode=? WHERE id=1", [mode],
        )?;
        ensure!(changed == 1, "manual_participation_missing_row");
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_state_survives_reopen_and_missing_row_is_not_reseeded() -> Result<()> {
        let store = Store::in_memory()?;
        assert!(store.take_manual_participation_initialized());
        assert!(!store.take_manual_participation_initialized());
        assert_eq!(store.manual_participation_mode()?, "normal");
        store.save_manual_participation("paused")?;
        let reopened = Store::initialize(store.db)?;
        assert!(!reopened.take_manual_participation_initialized());
        assert_eq!(reopened.manual_participation_mode()?, "paused");
        reopened.execute("DELETE FROM manual_participation WHERE id=1", [])?;
        assert!(Store::initialize(reopened.db).err()
            .is_some_and(|error| error.to_string().contains("manual_participation_missing_row")));
        Ok(())
    }
}
