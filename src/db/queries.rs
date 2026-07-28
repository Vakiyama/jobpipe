//! Higher-level query helpers built on the entity structs.

use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};

use super::entities::prelude::{Application, Company, Posting};
use super::entities::{application, company, posting};
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

/// Insert one company from `companies add`. Returns false if `(ats, slug)`
/// already exists (no duplicate created).
pub async fn add_company(
    db: &DatabaseConnection,
    name: &str,
    ats: &str,
    slug: &str,
    careers_url: Option<&str>,
) -> Result<bool> {
    let existing = Company::find()
        .filter(company::Column::Ats.eq(ats))
        .filter(company::Column::Slug.eq(slug))
        .one(db)
        .await?;
    if existing.is_some() {
        return Ok(false);
    }
    company::ActiveModel {
        name: Set(name.to_string()),
        ats: Set(ats.to_string()),
        slug: Set(slug.to_string()),
        careers_url: Set(careers_url.map(str::to_string)),
        tags: Set(None),
        active: Set(1),
        needs_review: Set(0),
        last_fetched: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(true)
}

/// All companies for `companies list`, optionally only those flagged for review.
pub async fn list_companies(
    db: &DatabaseConnection,
    needs_review_only: bool,
) -> Result<Vec<company::Model>> {
    let mut q = Company::find();
    if needs_review_only {
        q = q.filter(company::Column::NeedsReview.eq(1));
    }
    Ok(q.order_by_asc(company::Column::Ats)
        .order_by_asc(company::Column::Name)
        .all(db)
        .await?)
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

/// Untriaged, still-open postings awaiting a score. `triaged_at IS NULL` is the
/// contract from the spec — a posting is never re-scored once triaged.
pub async fn untriaged_postings(
    db: &DatabaseConnection,
    limit: Option<u64>,
) -> Result<Vec<posting::Model>> {
    let mut q = Posting::find()
        .filter(posting::Column::TriagedAt.is_null())
        .filter(posting::Column::ClosedAt.is_null())
        .order_by_asc(posting::Column::FirstSeen);
    if let Some(n) = limit {
        q = q.limit(n);
    }
    Ok(q.all(db).await?)
}

/// Persist a triage result on a posting: score, one-line reason, JSON flags
/// array, and the `triaged_at` stamp that removes it from future triage runs.
pub async fn set_triage(
    db: &DatabaseConnection,
    posting_id: i32,
    score: i32,
    reason: &str,
    flags_json: &str,
    now: &str,
) -> Result<()> {
    Posting::update_many()
        .col_expr(posting::Column::Score, Expr::value(score))
        .col_expr(posting::Column::ScoreReason, Expr::value(reason))
        .col_expr(posting::Column::Flags, Expr::value(flags_json))
        .col_expr(posting::Column::TriagedAt, Expr::value(now))
        .filter(posting::Column::Id.eq(posting_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Open postings for the digest, each paired with its company. When `min_score`
/// is set, only postings scored at or above it are returned (untriaged rows have
/// a NULL score and are excluded); results are ranked by score then recency.
pub async fn open_postings(
    db: &DatabaseConnection,
    since: Option<&str>,
    min_score: Option<i32>,
) -> Result<Vec<(posting::Model, Option<company::Model>)>> {
    let mut q = Posting::find().filter(posting::Column::ClosedAt.is_null());
    if let Some(s) = since {
        q = q.filter(posting::Column::FirstSeen.gte(s));
    }
    if let Some(m) = min_score {
        q = q.filter(posting::Column::Score.gte(m));
    }
    Ok(q.order_by_desc(posting::Column::Score)
        .order_by_desc(posting::Column::FirstSeen)
        .find_also_related(Company)
        .all(db)
        .await?)
}

/// Open postings that don't yet have an application, ranked like `open_postings`.
/// Backs the interactive `apply` picker so already-applied roles don't reappear;
/// `min_score` mirrors the digest threshold to keep the candidate set manageable.
pub async fn unapplied_open_postings(
    db: &DatabaseConnection,
    min_score: Option<i32>,
) -> Result<Vec<(posting::Model, Option<company::Model>)>> {
    let applied: Vec<i32> = Application::find()
        .select_only()
        .column(application::Column::PostingId)
        .into_tuple()
        .all(db)
        .await?;
    let mut q = Posting::find()
        .filter(posting::Column::ClosedAt.is_null())
        .filter(posting::Column::Id.is_not_in(applied));
    if let Some(m) = min_score {
        q = q.filter(posting::Column::Score.gte(m));
    }
    Ok(q.order_by_desc(posting::Column::Score)
        .order_by_desc(posting::Column::FirstSeen)
        .find_also_related(Company)
        .all(db)
        .await?)
}

/// A posting paired with its company, by posting id.
pub async fn posting_with_company(
    db: &DatabaseConnection,
    posting_id: i32,
) -> Result<Option<(posting::Model, Option<company::Model>)>> {
    let Some(p) = Posting::find_by_id(posting_id).one(db).await? else {
        return Ok(None);
    };
    let company = Company::find_by_id(p.company_id).one(db).await?;
    Ok(Some((p, company)))
}

/// Outcome of recording an application.
pub enum ApplyOutcome {
    Created(application::Model),
    Existing(application::Model),
}

/// Record an application against a posting. If one already exists for the
/// posting, returns it unchanged rather than duplicating (a role is applied to
/// once). The caller is expected to have validated the posting exists.
pub async fn create_application(
    db: &DatabaseConnection,
    posting_id: i32,
    now: &str,
) -> Result<ApplyOutcome> {
    if let Some(existing) = Application::find()
        .filter(application::Column::PostingId.eq(posting_id))
        .one(db)
        .await?
    {
        return Ok(ApplyOutcome::Existing(existing));
    }
    let model = application::ActiveModel {
        posting_id: Set(posting_id),
        applied_at: Set(now.to_string()),
        stage: Set("applied".to_string()),
        last_contact: Set(None),
        next_followup: Set(None),
        notes: Set(None),
        ..Default::default()
    }
    .insert(db)
    .await?;
    Ok(ApplyOutcome::Created(model))
}

/// An application joined to its posting and company for display.
pub type AppRow = (
    application::Model,
    Option<posting::Model>,
    Option<company::Model>,
);

/// List applications, newest first. With `stage`, filters to exactly that stage;
/// without it, returns only open applications (excludes rejected/ghosted).
pub async fn list_applications(
    db: &DatabaseConnection,
    stage: Option<&str>,
) -> Result<Vec<AppRow>> {
    let mut q = Application::find();
    q = match stage {
        Some(s) => q.filter(application::Column::Stage.eq(s)),
        None => q.filter(application::Column::Stage.is_not_in(["rejected", "ghosted"])),
    };
    let apps = q
        .order_by_desc(application::Column::AppliedAt)
        .all(db)
        .await?;

    let mut rows = Vec::with_capacity(apps.len());
    for a in apps {
        let posting = Posting::find_by_id(a.posting_id).one(db).await?;
        let company = match &posting {
            Some(p) => Company::find_by_id(p.company_id).one(db).await?,
            None => None,
        };
        rows.push((a, posting, company));
    }
    Ok(rows)
}

/// Move an application to a new stage, stamping `last_contact = now` (a logged
/// transition is a contact event, which resets the follow-up clocks) and
/// appending a dated note when one is given. Returns false if no such id.
pub async fn update_application_stage(
    db: &DatabaseConnection,
    app_id: i32,
    stage: &str,
    note: Option<&str>,
    now: &str,
    today: &str,
) -> Result<bool> {
    let Some(model) = Application::find_by_id(app_id).one(db).await? else {
        return Ok(false);
    };
    let mut am: application::ActiveModel = model.clone().into();
    am.stage = Set(stage.to_string());
    am.last_contact = Set(Some(now.to_string()));
    if let Some(n) = note {
        let appended = match model.notes {
            Some(prev) if !prev.is_empty() => format!("{prev}\n[{today}] {n}"),
            _ => format!("[{today}] {n}"),
        };
        am.notes = Set(Some(appended));
    }
    am.update(db).await?;
    Ok(true)
}
