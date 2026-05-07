//! Exclusion CRUD against `sync_dir_exclusions`.

use sea_orm::{ActiveValue, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::domain::{
    ports::{BoxFuture, RepositoryError},
    sync::{SyncDirExclusion, SyncDirExclusionId, SyncDirId},
};

use super::super::models::{
    SyncDirExclusionsActiveModel, SyncDirExclusionsColumn, SyncDirExclusionsEntity,
};
use super::map_err;

pub(super) fn list_exclusions(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
) -> BoxFuture<'_, Result<Vec<SyncDirExclusion>, RepositoryError>> {
    Box::pin(async move {
        let rows = SyncDirExclusionsEntity::find()
            .filter(SyncDirExclusionsColumn::SyncDirId.eq(sync_dir.0))
            .all(db)
            .await
            .map_err(map_err)?;
        Ok(rows
            .into_iter()
            .map(|m| SyncDirExclusion {
                id: SyncDirExclusionId(m.id),
                sync_dir_id: SyncDirId(m.sync_dir_id),
                remote_path: m.remote_path,
            })
            .collect())
    })
}

pub(super) fn insert_exclusion(
    db: &DatabaseConnection,
    sync_dir: SyncDirId,
    remote_path: String,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        let active = SyncDirExclusionsActiveModel {
            sync_dir_id: ActiveValue::Set(sync_dir.0),
            remote_path: ActiveValue::Set(remote_path),
            ..Default::default()
        };
        SyncDirExclusionsEntity::insert(active)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}

pub(super) fn delete_exclusion(
    db: &DatabaseConnection,
    id: SyncDirExclusionId,
) -> BoxFuture<'_, Result<(), RepositoryError>> {
    Box::pin(async move {
        SyncDirExclusionsEntity::delete_by_id(id.0)
            .exec(db)
            .await
            .map_err(map_err)?;
        Ok(())
    })
}
