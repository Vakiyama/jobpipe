use sea_orm::entity::prelude::*;

/// A company whose ATS board we poll. `UNIQUE(ats, slug)` — one board per row.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "companies")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    /// greenhouse | lever | ashby | ...
    pub ats: String,
    pub slug: String,
    pub careers_url: Option<String>,
    /// csv tags: vancouver, rust, remote-ca, ...
    pub tags: Option<String>,
    pub active: i32,
    pub needs_review: i32,
    /// RFC3339 timestamp of the last successful fetch.
    pub last_fetched: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Posting,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Relation::Posting => Entity::has_many(super::posting::Entity).into(),
        }
    }
}

impl Related<super::posting::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Posting.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
