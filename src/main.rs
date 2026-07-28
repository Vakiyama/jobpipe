mod cli;
mod config;
mod db;
mod migration;
mod normalize;
mod report;
mod sniff;
mod sources;
mod triage;

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use clap::Parser;
use futures::stream::{self, StreamExt};
use reqwest::Client;
use sea_orm::DatabaseConnection;
use std::io::IsTerminal;
use std::path::Path;
use tracing::{info, warn};

use cli::{Cli, Command, DigestFormat, Stage};
use db::entities::company;
use db::queries::{self, UpsertOutcome};
use sources::{source_for, FetchError};

/// A posting whose `last_seen` falls this far behind the run is marked closed.
const STALE_DAYS: i64 = 3;
/// Bounded concurrency for board fetches (spec: ~10 simultaneous requests).
const FETCH_CONCURRENCY: usize = 10;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env if present so secrets like ANTHROPIC_API_KEY can live in a file
    // instead of the shell. A real environment variable still wins over .env.
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Init { companies } => cmd_init(&cli.db, &companies).await,
        Command::Companies { action } => cmd_companies(&cli.db, action).await,
        Command::Fetch { only } => cmd_fetch(&cli.db, only.as_deref()).await,
        Command::Triage {
            limit,
            dry_run,
            profile,
        } => cmd_triage(&cli.db, limit, dry_run, &profile).await,
        Command::Apply { posting_id } => cmd_apply(&cli.db, posting_id).await,
        Command::Track { stage } => cmd_track(&cli.db, stage).await,
        Command::Stage {
            application_id,
            stage,
            note,
        } => cmd_stage(&cli.db, application_id, stage, note.as_deref()).await,
        Command::Followup => cmd_followup(&cli.db).await,
        Command::Digest {
            min_score,
            since,
            format,
            out,
        } => cmd_digest(&cli.db, min_score, since.as_deref(), format, out.as_deref()).await,
        Command::Run {
            min_score,
            out,
            profile,
        } => cmd_run(&cli.db, min_score, out.as_deref(), &profile).await,
    }
}

/// fetch + triage + digest, for cron. Triage failure (e.g. a missing key or a
/// transient API error) is logged but does not stop the digest from rendering
/// what's already scored.
async fn cmd_run(db_path: &str, min_score: i32, out: Option<&str>, profile: &str) -> Result<()> {
    cmd_fetch(db_path, None).await?;
    if let Err(e) = cmd_triage(db_path, None, false, profile).await {
        warn!("triage step failed, continuing to digest: {e:#}");
    }
    cmd_digest(db_path, min_score, None, DigestFormat::Term, out).await
}

/// Best-effort open of a URL in the user's browser. Never fails the command —
/// headless environments simply won't have an opener.
fn open_url(url: &str) {
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    let _ = std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn company_name(c: &Option<db::entities::company::Model>) -> &str {
    c.as_ref().map(|c| c.name.as_str()).unwrap_or("(unknown)")
}

/// Whole days elapsed from an RFC3339 timestamp to now.
fn days_since(ts: &str, now: chrono::DateTime<Utc>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| (now - t.with_timezone(&Utc)).num_days())
}

async fn cmd_apply(db_path: &str, posting_id: Option<i32>) -> Result<()> {
    let conn = db::connect(db_path).await?;
    let posting_id = match posting_id {
        Some(id) => id,
        None => match pick_posting(&conn).await? {
            Some(id) => id,
            None => return Ok(()), // nothing to pick, or the user cancelled
        },
    };
    let Some((posting, company)) = queries::posting_with_company(&conn, posting_id).await? else {
        anyhow::bail!("no posting with id {posting_id} — check `jobpipe digest`");
    };
    let now = Utc::now().to_rfc3339();
    match queries::create_application(&conn, posting_id, &now).await? {
        queries::ApplyOutcome::Existing(a) => {
            println!(
                "Already applied to \"{}\" — {} (application #{}, stage {}).",
                posting.title,
                company_name(&company),
                a.id,
                a.stage
            );
        }
        queries::ApplyOutcome::Created(a) => {
            println!(
                "Recorded application #{}: \"{}\" — {}.",
                a.id,
                posting.title,
                company_name(&company)
            );
            open_url(&posting.apply_url);
            println!("Apply: {}", posting.apply_url);
        }
    }
    Ok(())
}

/// Interactive posting picker for `apply` with no id. Returns the chosen
/// posting id, or `None` if there's nothing to pick or the user cancels.
async fn pick_posting(conn: &DatabaseConnection) -> Result<Option<i32>> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "no posting id given and stdin is not a terminal — pass an id, e.g. `jobpipe apply 3909`"
        );
    }
    let rows = queries::unapplied_open_postings(conn, Some(cli::DEFAULT_MIN_SCORE)).await?;
    if rows.is_empty() {
        println!(
            "No unapplied postings at score >= {} — apply by id with `jobpipe apply <id>`, \
             lower the bar via `jobpipe digest --min-score N`, or `jobpipe fetch` for more.",
            cli::DEFAULT_MIN_SCORE
        );
        return Ok(None);
    }
    let items: Vec<String> = rows
        .iter()
        .map(|(p, c)| {
            let score = p
                .score
                .map(|s| format!("[{s:>2}]"))
                .unwrap_or_else(|| "[ –]".to_string());
            format!("{score} #{} {} — {}", p.id, p.title, company_name(c))
        })
        .collect();
    let selection = dialoguer::FuzzySelect::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Pick a posting to apply to (type to filter)")
        .items(&items)
        .default(0)
        .max_length(15)
        .interact_opt()?;
    Ok(selection.map(|i| rows[i].0.id))
}

fn applied_date(ts: &str) -> &str {
    ts.split('T').next().unwrap_or(ts)
}

async fn cmd_track(db_path: &str, stage: Option<Stage>) -> Result<()> {
    let stage = stage.map(Stage::as_str);
    let conn = db::connect(db_path).await?;
    let rows = queries::list_applications(&conn, stage).await?;
    if rows.is_empty() {
        println!(
            "No {}applications.",
            stage.map(|s| format!("{s} ")).unwrap_or_default()
        );
        return Ok(());
    }
    for (a, posting, company) in &rows {
        let title = posting
            .as_ref()
            .map(|p| p.title.as_str())
            .unwrap_or("(posting gone)");
        println!(
            "app #{:<4} job #{:<5} {:<9} {}  —  {}  (applied {})",
            a.id,
            a.posting_id,
            a.stage,
            title,
            company_name(company),
            applied_date(&a.applied_at)
        );
    }
    println!("\n{} open application(s).", rows.len());
    Ok(())
}

async fn cmd_stage(
    db_path: &str,
    application_id: i32,
    stage: Stage,
    note: Option<&str>,
) -> Result<()> {
    let stage = stage.as_str();
    let conn = db::connect(db_path).await?;
    let now = Utc::now();
    let updated = queries::update_application_stage(
        &conn,
        application_id,
        stage,
        note,
        &now.to_rfc3339(),
        &now.format("%Y-%m-%d").to_string(),
    )
    .await?;
    if updated {
        println!("Application #{application_id} → {stage}.");
    } else {
        anyhow::bail!("no application with id {application_id} — check `jobpipe track`");
    }
    Ok(())
}

async fn cmd_followup(db_path: &str) -> Result<()> {
    let conn = db::connect(db_path).await?;
    let rows = queries::list_applications(&conn, None).await?;
    let now = Utc::now();
    let mut due = 0;
    for (a, posting, company) in &rows {
        let title = posting
            .as_ref()
            .map(|p| p.title.as_str())
            .unwrap_or("(posting gone)");
        let label = format!("#{} {} — {}", a.id, title, company_name(company));
        let suggestion = match a.stage.as_str() {
            // No contact since applying: nudge, then suggest giving up.
            "applied" if a.last_contact.is_none() => match days_since(&a.applied_at, now) {
                Some(d) if d >= 21 => Some(format!(
                    "{d}d since applied, no response — consider marking ghosted"
                )),
                Some(d) if d >= 7 => Some(format!(
                    "{d}d since applied, no response — send a follow-up email"
                )),
                _ => None,
            },
            // In an interview loop: check in if it's gone quiet.
            "screen" | "interview" => {
                let reference = a.last_contact.as_deref().unwrap_or(&a.applied_at);
                match days_since(reference, now) {
                    Some(d) if d >= 5 => Some(format!(
                        "{d}d since last contact in {} — send a check-in",
                        a.stage
                    )),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(s) = suggestion {
            println!("{label}\n    → {s}");
            due += 1;
        }
    }
    if due == 0 {
        println!("No follow-ups due.");
    } else {
        println!("\n{due} follow-up(s) due.");
    }
    Ok(())
}

async fn cmd_init(db_path: &str, companies_path: &str) -> Result<()> {
    let conn = db::connect(db_path).await?;
    db::run_migrations(&conn).await?;
    info!("migrations applied at {db_path}");

    let path = Path::new(companies_path);
    if !path.exists() {
        warn!("{companies_path} not found — database initialized with no companies");
        println!(
            "Initialized {db_path}. No {companies_path} found; add one and re-run init to seed."
        );
        return Ok(());
    }

    let seeds = config::load_companies(path)?;
    let inserted = queries::seed_companies(&conn, &seeds).await?;
    let total = queries::count_companies(&conn).await?;
    println!(
        "Initialized {db_path}. Seeded {inserted} new compan{} ({total} total).",
        if inserted == 1 { "y" } else { "ies" }
    );
    Ok(())
}

async fn cmd_companies(db_path: &str, action: cli::CompaniesAction) -> Result<()> {
    use cli::CompaniesAction;
    let conn = db::connect(db_path).await?;
    match action {
        CompaniesAction::Add(add) => {
            let client = sources::http_client();
            let detected = sniff::detect(&client, &add.url).await?;
            let inserted = queries::add_company(
                &conn,
                &detected.name,
                &detected.ats,
                &detected.slug,
                Some(&add.url),
            )
            .await?;
            if inserted {
                println!(
                    "Added {} — {}/{} ({}).",
                    detected.name, detected.ats, detected.slug, add.url
                );
            } else {
                println!(
                    "Already tracked: {}/{} — nothing added.",
                    detected.ats, detected.slug
                );
            }
        }
        CompaniesAction::List { needs_review } => {
            let companies = queries::list_companies(&conn, needs_review).await?;
            if companies.is_empty() {
                println!(
                    "No companies{}.",
                    if needs_review { " need review" } else { "" }
                );
                return Ok(());
            }
            for c in &companies {
                let flag = if c.needs_review == 1 {
                    " [needs-review]"
                } else {
                    ""
                };
                let tags = c.tags.as_deref().unwrap_or("");
                println!("{:<11} {:<22} {}{}", c.ats, c.slug, c.name, flag);
                if !tags.is_empty() {
                    println!("            tags: {tags}");
                }
            }
            println!(
                "\n{} compan{}.",
                companies.len(),
                if companies.len() == 1 { "y" } else { "ies" }
            );
        }
    }
    Ok(())
}

/// Per-company result of a fetch, so aggregation happens after the concurrent run.
enum Outcome {
    Fetched { new: u64, updated: u64 },
    Empty,
    NeedsReview,
    // Message is surfaced via `warn!` in fetch_one; retained for future summaries.
    Error(#[allow(dead_code)] String),
}

async fn cmd_fetch(db_path: &str, only: Option<&str>) -> Result<()> {
    let conn = db::connect(db_path).await?;
    let companies = queries::active_companies(&conn, only).await?;
    if companies.is_empty() {
        println!("No active companies to fetch. Run `jobpipe init` first.");
        return Ok(());
    }

    let client = sources::http_client();
    let now = Utc::now().to_rfc3339();
    info!("fetching {} board(s)", companies.len());

    let results: Vec<(company::Model, Outcome)> = stream::iter(companies)
        .map(|company| {
            let conn = conn.clone();
            let client = client.clone();
            let now = now.clone();
            async move {
                let outcome = fetch_one(&conn, &client, &company, &now).await;
                (company, outcome)
            }
        })
        .buffer_unordered(FETCH_CONCURRENCY)
        .collect()
        .await;

    // Aggregate and decide which boards' postings are eligible for stale-closing.
    let mut total_new = 0u64;
    let mut total_updated = 0u64;
    let mut fetched_ids: Vec<i32> = Vec::new();
    let mut review = 0u64;
    let mut errors = 0u64;

    for (company, outcome) in &results {
        match outcome {
            Outcome::Fetched { new, updated } => {
                total_new += new;
                total_updated += updated;
                fetched_ids.push(company.id);
            }
            Outcome::Empty => {
                fetched_ids.push(company.id);
                review += 1;
            }
            Outcome::NeedsReview => review += 1,
            Outcome::Error(_) => errors += 1,
        }
    }

    let cutoff = (Utc::now() - Duration::days(STALE_DAYS)).to_rfc3339();
    let closed = queries::close_stale_postings(&conn, &fetched_ids, &cutoff, &now).await?;

    println!(
        "Fetch complete: {total_new} new, {total_updated} updated, {closed} closed, \
         {review} needs-review, {errors} error(s)."
    );
    Ok(())
}

/// Fetch, normalize, and upsert one company's board. Never returns Err — every
/// failure is captured in [`Outcome`] so one bad board can't abort the run.
async fn fetch_one(
    conn: &DatabaseConnection,
    client: &Client,
    company: &company::Model,
    now: &str,
) -> Outcome {
    let Some(source) = source_for(&company.ats, client) else {
        return Outcome::Error(format!("no source for ats '{}'", company.ats));
    };

    match source.fetch(&company.slug).await {
        Ok(raw) if raw.is_empty() => {
            warn!(company = %company.name, "empty board — marking needs_review");
            let _ = queries::mark_needs_review(conn, company.id).await;
            let _ = queries::touch_last_fetched(conn, company.id, now).await;
            Outcome::Empty
        }
        Ok(raw) => {
            let mut new = 0;
            let mut updated = 0;
            for raw_posting in raw {
                let normalized = normalize::normalize(raw_posting);
                match queries::upsert_posting(conn, company.id, &normalized, now).await {
                    Ok(UpsertOutcome::Inserted) => new += 1,
                    Ok(UpsertOutcome::Updated) => updated += 1,
                    Err(e) => warn!(company = %company.name, error = %e, "upsert failed"),
                }
            }
            let _ = queries::touch_last_fetched(conn, company.id, now).await;
            info!(company = %company.name, new, updated, "fetched");
            Outcome::Fetched { new, updated }
        }
        Err(FetchError::NotFound) => {
            warn!(company = %company.name, slug = %company.slug, "board 404 — marking needs_review");
            let _ = queries::mark_needs_review(conn, company.id).await;
            Outcome::NeedsReview
        }
        Err(e) => {
            warn!(company = %company.name, error = %e, "fetch failed — skipping");
            Outcome::Error(e.to_string())
        }
    }
}

async fn cmd_triage(
    db_path: &str,
    limit: Option<u64>,
    dry_run: bool,
    profile_path: &str,
) -> Result<()> {
    let conn = db::connect(db_path).await?;
    let path = Path::new(profile_path);
    if !path.exists() {
        anyhow::bail!("{profile_path} not found — it holds the candidate profile for scoring");
    }
    let profile_text = config::load_profile_text(path)?;
    triage::run(&conn, limit, dry_run, &profile_text).await
}

async fn cmd_digest(
    db_path: &str,
    min_score: i32,
    since: Option<&str>,
    format: DigestFormat,
    out: Option<&str>,
) -> Result<()> {
    let conn = db::connect(db_path).await?;
    let cutoff = match since {
        Some(s) => Some(parse_since(s)?),
        None => None,
    };
    let rows = queries::open_postings(&conn, cutoff.as_deref(), Some(min_score)).await?;

    // A file report is always markdown; stdout honors --format.
    if let Some(path) = out {
        let md = report::render(&rows, report::Format::Md, false);
        std::fs::write(path, md).with_context(|| format!("writing digest to {path}"))?;
        println!("Wrote {} posting(s) to {path}.", rows.len());
    } else {
        let format = match format {
            DigestFormat::Term => report::Format::Term,
            DigestFormat::Md => report::Format::Md,
        };
        // Clickable apply links only when stdout is a real terminal, so the
        // escape sequences never leak into pipes or redirects.
        let hyperlinks = std::io::stdout().is_terminal();
        print!("{}", report::render(&rows, format, hyperlinks));
    }
    Ok(())
}

/// Parse a `--since` window like `1d`, `3d`, `12h`, `2w` into an RFC3339 cutoff.
/// A bare number is interpreted as days.
fn parse_since(s: &str) -> Result<String> {
    let s = s.trim();
    let (num_str, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num_str
        .parse()
        .with_context(|| format!("invalid --since value '{s}'"))?;
    let dur = match unit {
        "" | "d" => Duration::days(n),
        "h" => Duration::hours(n),
        "w" => Duration::weeks(n),
        other => anyhow::bail!("unknown --since unit '{other}' (use h, d, or w)"),
    };
    Ok((Utc::now() - dur).to_rfc3339())
}
