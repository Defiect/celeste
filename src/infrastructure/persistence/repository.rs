//! SeaORM-backed implementation of [`crate::domain::ports::Repository`].

use std::{path::PathBuf, time::Duration};

use sea_orm::{ActiveValue, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::domain::{
    ports::{BoxFuture, Repository, RepositoryError},
    remote::{Remote, RemoteId, SyncPolicy},
    sync::{SyncDir, SyncDirId, SyncItem},
};

use super::models::{
    RemotesActiveModel, RemotesEntity, RemotesModel, SyncDirsColumn, SyncDirsEntity,
    SyncDirsModel, SyncItemsColumn, SyncItemsEntity, SyncItemsModel,
};

pub struct SeaOrmRepository {
    db: DatabaseConnection,
}

impl SeaOrmRepository {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }
}

fn map_remote(m: RemotesModel) -> Remote {
    Remote {
        id: RemoteId(m.id),
        name: m.name,
        policy: SyncPolicy {
            interval: Duration::from_secs(m.sync_interval_seconds.max(1) as u64),
            instant_sync: m.instant_sync != 0,
            enabled: m.enabled != 0,
        },
    }
}

fn map_sync_dir(m: SyncDirsModel) -> SyncDir {
    SyncDir {
        id: SyncDirId(m.id),
        remote_id: RemoteId(m.remote_id),
        local_path: PathBuf::from(m.local_path),
        remote_path: m.remote_path,
    }
}

fn map_sync_item(m: SyncItemsModel) -> SyncItem {
    SyncItem {
        sync_dir_id: SyncDirId(m.sync_dir_id),
        local_path: PathBuf::from(m.local_path),
        remote_path: m.remote_path,
        last_local_timestamp: m.last_local_timestamp as i64,
        last_remote_timestamp: m.last_remote_timestamp as i64,
    }
}

fn map_err<E: std::fmt::Display>(err: E) -> RepositoryError {
    RepositoryError::Other(err.to_string())
}

impl Repository for SeaOrmRepository {
    fn list_remotes(&self) -> BoxFuture<'_, Result<Vec<Remote>, RepositoryError>> {
        Box::pin(async move {
            let rows = RemotesEntity::find()
                .all(&self.db)
                .await
                .map_err(map_err)?;
            Ok(rows.into_iter().map(map_remote).collect())
        })
    }

    fn find_remote(
        &self,
        id: RemoteId,
    ) -> BoxFuture<'_, Result<Option<Remote>, RepositoryError>> {
        Box::pin(async move {
            let row = RemotesEntity::find_by_id(id.0)
                .one(&self.db)
                .await
                .map_err(map_err)?;
            Ok(row.map(map_remote))
        })
    }

    fn delete_remote(&self, id: RemoteId) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async move {
            RemotesEntity::delete_by_id(id.0)
                .exec(&self.db)
                .await
                .map_err(map_err)?;
            Ok(())
        })
    }

    fn set_policy(
        &self,
        id: RemoteId,
        policy: SyncPolicy,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        Box::pin(async move {
            let active = RemotesActiveModel {
                id: ActiveValue::Unchanged(id.0),
                sync_interval_seconds: ActiveValue::Set(policy.interval.as_secs() as i32),
                instant_sync: ActiveValue::Set(policy.instant_sync as i32),
                enabled: ActiveValue::Set(policy.enabled as i32),
                ..Default::default()
            };
            RemotesEntity::update(active)
                .exec(&self.db)
                .await
                .map_err(map_err)?;
            Ok(())
        })
    }

    fn list_sync_dirs(
        &self,
        remote: RemoteId,
    ) -> BoxFuture<'_, Result<Vec<SyncDir>, RepositoryError>> {
        Box::pin(async move {
            let rows = SyncDirsEntity::find()
                .filter(SyncDirsColumn::RemoteId.eq(remote.0))
                .all(&self.db)
                .await
                .map_err(map_err)?;
            Ok(rows.into_iter().map(map_sync_dir).collect())
        })
    }

    fn sync_dir_exists(
        &self,
        local_path: &str,
        remote_path: &str,
    ) -> BoxFuture<'_, Result<bool, RepositoryError>> {
        let local = local_path.to_owned();
        let remote = remote_path.to_owned();
        Box::pin(async move {
            let row = SyncDirsEntity::find()
                .filter(SyncDirsColumn::LocalPath.eq(local))
                .filter(SyncDirsColumn::RemotePath.eq(remote))
                .one(&self.db)
                .await
                .map_err(map_err)?;
            Ok(row.is_some())
        })
    }

    fn list_sync_items(
        &self,
        sync_dir: SyncDirId,
    ) -> BoxFuture<'_, Result<Vec<SyncItem>, RepositoryError>> {
        Box::pin(async move {
            let rows = SyncItemsEntity::find()
                .filter(SyncItemsColumn::SyncDirId.eq(sync_dir.0))
                .all(&self.db)
                .await
                .map_err(map_err)?;
            Ok(rows.into_iter().map(map_sync_item).collect())
        })
    }
}
