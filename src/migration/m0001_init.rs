use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Companies::Table)
                    .if_not_exists()
                    .col(pk_auto(Companies::Id))
                    .col(string(Companies::Name))
                    .col(string(Companies::Ats))
                    .col(string(Companies::Slug))
                    .col(string_null(Companies::CareersUrl))
                    .col(string_null(Companies::Tags))
                    .col(integer(Companies::Active).default(1))
                    .col(integer(Companies::NeedsReview).default(0))
                    .col(string_null(Companies::LastFetched))
                    .index(
                        Index::create()
                            .name("uq_companies_ats_slug")
                            .col(Companies::Ats)
                            .col(Companies::Slug)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Postings::Table)
                    .if_not_exists()
                    .col(pk_auto(Postings::Id))
                    .col(integer(Postings::CompanyId))
                    .col(string(Postings::ExternalId))
                    .col(string(Postings::Title))
                    .col(string_null(Postings::Location))
                    .col(string_null(Postings::Remote))
                    .col(text(Postings::Description))
                    .col(string(Postings::ApplyUrl))
                    .col(string(Postings::FirstSeen))
                    .col(string(Postings::LastSeen))
                    .col(string_null(Postings::ClosedAt))
                    .col(integer_null(Postings::Score))
                    .col(string_null(Postings::ScoreReason))
                    .col(string_null(Postings::Flags))
                    .col(string_null(Postings::TriagedAt))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_postings_company")
                            .from(Postings::Table, Postings::CompanyId)
                            .to(Companies::Table, Companies::Id),
                    )
                    .index(
                        Index::create()
                            .name("uq_postings_company_external")
                            .col(Postings::CompanyId)
                            .col(Postings::ExternalId)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Applications::Table)
                    .if_not_exists()
                    .col(pk_auto(Applications::Id))
                    .col(integer(Applications::PostingId))
                    .col(string(Applications::AppliedAt))
                    .col(string(Applications::Stage).default("applied"))
                    .col(string_null(Applications::LastContact))
                    .col(string_null(Applications::NextFollowup))
                    .col(string_null(Applications::Notes))
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_applications_posting")
                            .from(Applications::Table, Applications::PostingId)
                            .to(Postings::Table, Postings::Id),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Applications::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Postings::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Companies::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Companies {
    Table,
    Id,
    Name,
    Ats,
    Slug,
    CareersUrl,
    Tags,
    Active,
    NeedsReview,
    LastFetched,
}

#[derive(DeriveIden)]
enum Postings {
    Table,
    Id,
    CompanyId,
    ExternalId,
    Title,
    Location,
    Remote,
    Description,
    ApplyUrl,
    FirstSeen,
    LastSeen,
    ClosedAt,
    Score,
    ScoreReason,
    Flags,
    TriagedAt,
}

#[derive(DeriveIden)]
enum Applications {
    Table,
    Id,
    PostingId,
    AppliedAt,
    Stage,
    LastContact,
    NextFollowup,
    Notes,
}
