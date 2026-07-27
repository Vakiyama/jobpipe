use sea_orm::entity::prelude::*;

/// A recorded application against a posting. Drives the follow-up tracker (phase 4).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "applications")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub posting_id: i32,
    pub applied_at: String,
    /// applied | screen | interview | offer | rejected | ghosted
    pub stage: String,
    pub last_contact: Option<String>,
    pub next_followup: Option<String>,
    pub notes: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Posting,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Relation::Posting => Entity::belongs_to(super::posting::Entity)
                .from(Column::PostingId)
                .to(super::posting::Column::Id)
                .into(),
        }
    }
}

impl Related<super::posting::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Posting.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
