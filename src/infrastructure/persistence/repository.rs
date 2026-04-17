//! SeaORM-backed implementation of [`crate::domain::ports::Repository`].

use std::path::PathBuf;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::domain::{
    ports::{BoxFuture, Repository, RepositoryError},
    remote::{Remote, RemoteId, SyncPolicy},
    sync::{SyncDir, SyncDirId, SyncItem},
};

use super::models::{
    RemotesEntity, RemotesModel, SyncDirsColumn, SyncDirsEntity, SyncDirsModel, SyncItemsColumn,
    SyncItemsEntity, SyncItemsModel,
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
    // Policy columns arrive in the Phase B migration; until then every remote
    // gets the defaults.
    Remote {
        id: RemoteId(m.id),
        name: m.name,
        policy: SyncPolicy::default(),
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
        _id: RemoteId,
        _policy: SyncPolicy,
    ) -> BoxFuture<'_, Result<(), RepositoryError>> {
        // Phase B adds the sync_policy columns and the real UPDATE here.
        Box::pin(async { Ok(()) })
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
