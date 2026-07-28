use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

/// Adds `dismissed_at` to postings: a user-set "hide from the digest and apply
/// picker" flag, kept separate from the LLM `score` so dismissing never
/// masquerades as a triage result. Null means not dismissed.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Postings::Table)
                    .add_column(string_null(Postings::DismissedAt))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Postings::Table)
                    .drop_column(Postings::DismissedAt)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Postings {
    Table,
    DismissedAt,
}
