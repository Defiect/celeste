use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = r#"
            ALTER TABLE remotes ADD COLUMN sync_interval_seconds INTEGER NOT NULL DEFAULT 300;
            ALTER TABLE remotes ADD COLUMN instant_sync INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE remotes ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;
            ALTER TABLE remotes ADD COLUMN last_sync_at INTEGER;
            ALTER TABLE remotes ADD COLUMN last_sync_status TEXT;
        "#;
        let stmt = Statement::from_string(manager.get_database_backend(), sql.to_owned());
        manager.get_connection().execute(stmt).await.map(|_| ())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = r#"
            ALTER TABLE remotes DROP COLUMN last_sync_status;
            ALTER TABLE remotes DROP COLUMN last_sync_at;
            ALTER TABLE remotes DROP COLUMN enabled;
            ALTER TABLE remotes DROP COLUMN instant_sync;
            ALTER TABLE remotes DROP COLUMN sync_interval_seconds;
        "#;
        let stmt = Statement::from_string(manager.get_database_backend(), sql.to_owned());
        manager.get_connection().execute(stmt).await.map(|_| ())
    }
}
