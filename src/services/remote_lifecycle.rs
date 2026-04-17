//! Remove-a-remote workflow. Deletes every sync_item / sync_dir / remote
//! row for the given remote and then drops the matching rclone config.
//!
//! Unlike the sync algorithm, this one has no UI coupling left: the caller
//! (the GTK main loop today) removes the UI entry itself before or after
//! calling this.

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, ModelTrait, QueryFilter};

use crate::{
    domain::ports::RcloneClient,
    infrastructure::persistence::models::{
        RemotesColumn, RemotesEntity, SyncDirsColumn, SyncDirsEntity, SyncItemsColumn,
        SyncItemsEntity,
    },
    util,
};

/// Cascade-delete one sync_dir (by local + remote path) and all of its
/// sync_items from the DB. UI unlinking is the caller's problem.
pub fn delete_sync_dir(
    local_path: &str,
    remote_path: &str,
    db: &DatabaseConnection,
) -> Result<(), String> {
    util::await_future(async {
        let sync_dir = SyncDirsEntity::find()
            .filter(SyncDirsColumn::LocalPath.eq(local_path))
            .filter(SyncDirsColumn::RemotePath.eq(remote_path))
            .one(db)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                format!("sync_dir '{local_path}' <-> '{remote_path}' not found")
            })?;
        SyncItemsEntity::delete_many()
            .filter(SyncItemsColumn::SyncDirId.eq(sync_dir.id))
            .exec(db)
            .await
            .map_err(|e| e.to_string())?;
        sync_dir.delete(db).await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })
}

pub fn delete_remote(
    remote_name: &str,
    db: &DatabaseConnection,
    client: &dyn RcloneClient,
) -> Result<(), String> {
    util::await_future(async {
        let db_remote = RemotesEntity::find()
            .filter(RemotesColumn::Name.eq(remote_name))
            .one(db)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("remote '{remote_name}' not found"))?;
        let sync_dirs = SyncDirsEntity::find()
            .filter(SyncDirsColumn::RemoteId.eq(db_remote.id))
            .all(db)
            .await
            .map_err(|e| e.to_string())?;
        for sd in sync_dirs {
            SyncItemsEntity::delete_many()
                .filter(SyncItemsColumn::SyncDirId.eq(sd.id))
                .exec(db)
                .await
                .map_err(|e| e.to_string())?;
            sd.delete(db).await.map_err(|e| e.to_string())?;
        }
        db_remote.delete(db).await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })?;

    client.delete_config(remote_name)
}
