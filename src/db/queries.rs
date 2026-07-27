//! Higher-level query helpers built on the entity structs.

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};

use super::entities::prelude::{Company, Posting};
use super::entities::{company, posting};
use crate::normalize::NormalizedPosting;

/// A company to seed from `companies.toml`.
pub struct SeedCompany {
    pub name: String,
    pub ats: String,
    pub slug: String,
    pub careers_url: Option<String>,
    pub tags: Option<String>,
}

/// Whether an upsert created a new row or refreshed an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Inserted,
    Updated,
}

/// Insert seed companies, ignoring any that already exist on `(ats, slug)`.
/// Returns the number of rows actually inserted.
pub async fn seed_companies(db: &DatabaseConnection, seeds: &[SeedCompany]) -> Result<u64> {
    if seeds.is_empty() {
        return Ok(0);
    }
    let before = Company::find().all(db).await?.len() as u64;
    let models: Vec<company::ActiveModel> = seeds
        .iter()
        .map(|s| company::ActiveModel {
            name: Set(s.name.clone()),
            ats: Set(s.ats.clone()),
            slug: Set(s.slug.clone()),
            careers_url: Set(s.careers_url.clone()),
            tags: Set(s.tags.clone()),
            active: Set(1),
            needs_review: Set(0),
            last_fetched: Set(None),
            ..Default::default()
        })
        .collect();

    Company::insert_many(models)
        .on_conflict(
            OnConflict::columns([company::Column::Ats, company::Column::Slug])
                .do_nothing()
                .to_owned(),
        )
        .do_nothing()
        .exec(db)
        .await
        .context("seeding companies")?;

    let after = Company::find().all(db).await?.len() as u64;
    Ok(after - before)
}

/// All active companies, optionally filtered to a single ATS.
pub async fn active_companies(
    db: &DatabaseConnection,
    only_ats: Option<&str>,
) -> Result<Vec<company::Model>> {
    let mut q = Company::find().filter(company::Column::Active.eq(1));
    if let Some(ats) = only_ats {
        q = q.filter(company::Column::Ats.eq(ats));
    }
    Ok(q.order_by_asc(company::Column::Name).all(db).await?)
}

pub async fn count_companies(db: &DatabaseConnection) -> Result<u64> {
    Ok(Company::find().all(db).await?.len() as u64)
}

/// Flag a company for manual review (e.g. its board 404'd — likely an ATS change).
pub async fn mark_needs_review(db: &DatabaseConnection, company_id: i32) -> Result<()> {
    Company::update_many()
        .col_expr(company::Column::NeedsReview, Expr::value(1))
        .filter(company::Column::Id.eq(company_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Record a successful fetch timestamp on a company.
pub async fn touch_last_fetched(db: &DatabaseConnection, company_id: i32, now: &str) -> Result<()> {
    Company::update_many()
        .col_expr(company::Column::LastFetched, Expr::value(now))
        .filter(company::Column::Id.eq(company_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Upsert one normalized posting. New rows get `first_seen = last_seen = now`;
/// existing rows have their volatile fields and `last_seen` refreshed and any
/// prior `closed_at` cleared (the role reappeared). `first_seen`, `score`, and
/// triage columns are never touched here.
pub async fn upsert_posting(
    db: &DatabaseConnection,
    company_id: i32,
    p: &NormalizedPosting,
    now: &str,
) -> Result<UpsertOutcome> {
    let existing = Posting::find()
        .filter(posting::Column::CompanyId.eq(company_id))
        .filter(posting::Column::ExternalId.eq(p.external_id.as_str()))
        .one(db)
        .await?;

    match existing {
        Some(model) => {
            let mut am: posting::ActiveModel = model.into();
            am.title = Set(p.title.clone());
            am.location = Set(p.location.clone());
            am.remote = Set(Some(p.remote.clone()));
            am.description = Set(p.description.clone());
            am.apply_url = Set(p.apply_url.clone());
            am.last_seen = Set(now.to_string());
            am.closed_at = Set(None);
            am.update(db).await?;
            Ok(UpsertOutcome::Updated)
        }
        None => {
            let am = posting::ActiveModel {
                company_id: Set(company_id),
                external_id: Set(p.external_id.clone()),
                title: Set(p.title.clone()),
                location: Set(p.location.clone()),
                remote: Set(Some(p.remote.clone())),
                description: Set(p.description.clone()),
                apply_url: Set(p.apply_url.clone()),
                first_seen: Set(now.to_string()),
                last_seen: Set(now.to_string()),
                ..Default::default()
            };
            am.insert(db).await?;
            Ok(UpsertOutcome::Inserted)
        }
    }
}

/// Mark postings closed when they've fallen more than the cutoff behind the run.
/// Scoped to the companies we actually fetched this run so a skipped board
/// doesn't close everything under it. Returns rows affected.
pub async fn close_stale_postings(
    db: &DatabaseConnection,
    company_ids: &[i32],
    cutoff: &str,
    now: &str,
) -> Result<u64> {
    if company_ids.is_empty() {
        return Ok(0);
    }
    let res = Posting::update_many()
        .col_expr(posting::Column::ClosedAt, Expr::value(now))
        .filter(posting::Column::ClosedAt.is_null())
        .filter(posting::Column::LastSeen.lt(cutoff))
        .filter(posting::Column::CompanyId.is_in(company_ids.iter().copied()))
        .exec(db)
        .await?;
    Ok(res.rows_affected)
}

/// Open postings for the digest, newest first, each paired with its company.
pub async fn open_postings(
    db: &DatabaseConnection,
    since: Option<&str>,
) -> Result<Vec<(posting::Model, Option<company::Model>)>> {
    let mut q = Posting::find().filter(posting::Column::ClosedAt.is_null());
    if let Some(s) = since {
        q = q.filter(posting::Column::FirstSeen.gte(s));
    }
    Ok(q.order_by_desc(posting::Column::FirstSeen)
        .find_also_related(Company)
        .all(db)
        .await?)
}
