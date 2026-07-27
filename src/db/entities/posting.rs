use sea_orm::entity::prelude::*;

/// A single open role. Dedup key is `UNIQUE(company_id, external_id)`.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "postings")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub company_id: i32,
    /// ATS-native id.
    pub external_id: String,
    pub title: String,
    pub location: Option<String>,
    /// remote | hybrid | onsite | unknown
    pub remote: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub description: String,
    pub apply_url: String,
    pub first_seen: String,
    pub last_seen: String,
    pub closed_at: Option<String>,
    /// 0-10, null until triaged.
    pub score: Option<i32>,
    pub score_reason: Option<String>,
    /// json array
    pub flags: Option<String>,
    pub triaged_at: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Company,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Relation::Company => Entity::belongs_to(super::company::Entity)
                .from(Column::CompanyId)
                .to(super::company::Column::Id)
                .into(),
        }
    }
}

impl Related<super::company::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Company.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
