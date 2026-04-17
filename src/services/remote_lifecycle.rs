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
