use clap::{Args, Parser, Subcommand, ValueEnum};

/// Default digest score threshold. Also the floor for the interactive `apply`
/// picker, so it offers the same postings the digest surfaces.
pub const DEFAULT_MIN_SCORE: i32 = 7;

#[derive(Debug, Parser)]
#[command(
    name = "jobpipe",
    about = "Daily ranked digest of new job postings from company ATS boards",
    version
)]
pub struct Cli {
    /// Path to the SQLite database file.
    #[arg(long, global = true, env = "JOBPIPE_DB", default_value = "jobpipe.db")]
    pub db: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create the database, run migrations, and seed companies from companies.toml.
    Init {
        /// Company seed file.
        #[arg(long, default_value = "companies.toml")]
        companies: String,
    },

    /// Manage the company list.
    Companies {
        #[command(subcommand)]
        action: CompaniesAction,
    },

    /// Poll all active boards and upsert postings.
    Fetch {
        /// Restrict to a single ATS (e.g. greenhouse).
        #[arg(long)]
        only: Option<String>,
    },

    /// Score untriaged postings against the candidate profile via the LLM.
    Triage {
        /// Cap the number of postings considered this run.
        #[arg(long)]
        limit: Option<u64>,

        /// Report how many postings would be sent and an estimated token count,
        /// without calling the API or spending anything.
        #[arg(long)]
        dry_run: bool,

        /// Candidate profile file.
        #[arg(long, default_value = "profile.toml")]
        profile: String,
    },

    /// Record an application against a posting and open its apply URL.
    ///
    /// With no id, opens an interactive picker over the open postings.
    Apply {
        /// The posting id (from the digest). Omit to pick interactively.
        posting_id: Option<i32>,
    },

    /// List open applications, optionally filtered to one stage.
    Track {
        /// Show only this stage.
        #[arg(long, value_enum)]
        stage: Option<Stage>,
    },

    /// Move an application to a new stage.
    Stage {
        /// The application id (from `track`).
        application_id: i32,
        /// The new stage.
        #[arg(value_enum)]
        stage: Stage,
        /// A note to append to the application, dated.
        #[arg(long)]
        note: Option<String>,
    },

    /// List applications that are due for a nudge.
    Followup,

    /// Print the digest of open postings.
    Digest {
        /// Only show postings scored at or above this threshold.
        #[arg(long, default_value_t = DEFAULT_MIN_SCORE)]
        min_score: i32,

        /// Only postings first seen within this window, e.g. `1d`, `3d`, `12h`.
        #[arg(long)]
        since: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value_t = DigestFormat::Term)]
        format: DigestFormat,

        /// Write the digest as markdown to this file instead of stdout.
        #[arg(long)]
        out: Option<String>,
    },

    /// fetch + triage + digest in one shot, for a cron job.
    Run {
        /// Digest threshold.
        #[arg(long, default_value_t = DEFAULT_MIN_SCORE)]
        min_score: i32,

        /// Write the markdown digest to this file (otherwise printed).
        #[arg(long)]
        out: Option<String>,

        /// Candidate profile file for triage.
        #[arg(long, default_value = "profile.toml")]
        profile: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum CompaniesAction {
    /// Detect the ATS from a careers URL and insert the company.
    Add(CompaniesAdd),
    /// List companies, optionally only those needing review.
    List {
        #[arg(long)]
        needs_review: bool,
    },
}

#[derive(Debug, Args)]
pub struct CompaniesAdd {
    /// A careers or job-board URL (e.g. https://jobs.lever.co/acme).
    pub url: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DigestFormat {
    Term,
    Md,
}

/// The canonical application stages, in order. Single source of truth: clap
/// validates CLI input against these variants and renders them in `--help`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Stage {
    Applied,
    Screen,
    Interview,
    Offer,
    Rejected,
    Ghosted,
}

impl Stage {
    /// The canonical lowercase string stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Applied => "applied",
            Stage::Screen => "screen",
            Stage::Interview => "interview",
            Stage::Offer => "offer",
            Stage::Rejected => "rejected",
            Stage::Ghosted => "ghosted",
        }
    }
}
