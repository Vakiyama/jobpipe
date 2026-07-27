use clap::{Args, Parser, Subcommand, ValueEnum};

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

    /// Print the digest of open postings.
    Digest {
        /// Only show postings scored at or above this threshold.
        #[arg(long, default_value_t = 7)]
        min_score: i32,

        /// Only postings first seen within this window, e.g. `1d`, `3d`, `12h`.
        #[arg(long)]
        since: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value_t = DigestFormat::Term)]
        format: DigestFormat,
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
