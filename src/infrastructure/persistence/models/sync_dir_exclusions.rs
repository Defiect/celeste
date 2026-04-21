use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "sync_dir_exclusions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub sync_dir_id: i32,
    /// Remote sub-path relative to the owning sync_dir's `remote_path`.
    /// E.g. `"Videos"` when the sync_dir's remote_path is `"My files"`.
    pub remote_path: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::sync_dirs::Entity",
        from = "Column::SyncDirId",
        to = "super::sync_dirs::Column::Id",
        on_update = "NoAction",
        on_delete = "NoAction"
    )]
    SyncDirs,
}

impl Related<super::sync_dirs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SyncDirs.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
