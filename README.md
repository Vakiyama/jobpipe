# jobpipe

A single-binary CLI that pulls new job postings straight from companies' ATS
boards, scores each one against **your** profile with an LLM, and gives you a
daily ranked digest — plus a lightweight pipeline to apply, track, and follow up.

No scraping of aggregator UIs, no browser automation: it hits the same JSON
endpoints the career pages themselves use, stores everything in a local SQLite
file, and only ever sends a posting to the LLM once.

```
$ jobpipe run
Fetch complete: 37 new, 112 updated, 4 closed, 1 needs-review, 0 error(s).
Scoring 37 posting(s) in 3 batch(es) via claude-haiku-4-5 …
Triage complete: 9 pre-filtered, 28 scored, 0 failed.

  [10] #3912 Senior Backend Engineer (Rust) — Acme  ·  production Rust role, remote-Canada, exact stack match
  [ 9] #3877 Full Stack Engineer — Globex  ·  strong TS/React match in band, US & Canada remote
  ...
```

## How it works

1. **fetch** — polls every active board concurrently and upserts postings into
   SQLite. Postings that disappear from a board are marked closed after a few days.
2. **triage** — untriaged postings pass a cheap code-side pre-filter (obvious-reject
   titles), then survivors are batched to the Anthropic Messages API and scored
   0–10 against your profile. A posting is scored **once**; the system prompt is
   prompt-cached across batches to keep cost down. Use `--dry-run` to preview the
   token spend before paying.
3. **digest** — prints the open postings at or above a score threshold, newest first,
   with clickable apply links in the terminal (or writes markdown to a file).
4. **apply / track / stage / followup** — record an application, move it through
   stages (`applied → screen → interview → offer / rejected / ghosted`), and get
   nudges when something's gone quiet.

`jobpipe run` chains fetch + triage + digest in one shot — point cron at it.

### Supported sources

Per-company boards: **Greenhouse**, **Lever**, **Ashby**, **Workable**, **Recruitee**.
Aggregators: **RemoteOK**, **Remotive**, **Hacker News "Who is hiring?"**.

`jobpipe companies add <careers-url>` auto-detects which ATS a company uses.

## Install

### With Nix, without cloning (easiest)

The seed company list and profile template are baked into the binary, so you can
run jobpipe straight from GitHub — no clone needed. `jobpipe setup` writes starter
`profile.toml` and `companies.toml` into whatever directory you're in:

```sh
mkdir jobsearch && cd jobsearch
nix run github:Vakiyama/jobpipe -- setup      # writes config here
$EDITOR profile.toml                               # describe yourself
export ANTHROPIC_API_KEY=sk-ant-...                # needed only for triage
nix run github:Vakiyama/jobpipe -- init       # create the db + seed companies
nix run github:Vakiyama/jobpipe -- fetch
nix run github:Vakiyama/jobpipe -- triage
nix run github:Vakiyama/jobpipe -- digest
```

An alias keeps it short: `alias jobpipe='nix run github:Vakiyama/jobpipe --'`,
then just `jobpipe fetch`, `jobpipe digest`, etc. (`init` even works with no
`companies.toml` — it falls back to the built-in list.)

### With Nix, from a clone

```sh
git clone https://github.com/Vakiyama/jobpipe
cd jobpipe
nix run . -- --help
```

For a dev shell with the full Rust toolchain (rustc, cargo, clippy, rust-analyzer):

```sh
nix develop
```

Inside the dev shell, `jobpipe` is on your `PATH` as a wrapper that runs the
**optimized release build** (`cargo run --release`), rebuilding incrementally only
when the source changes. So you can just type `jobpipe fetch`, `jobpipe digest`,
etc. instead of `cargo run`. (Using [direnv](https://direnv.net/)? The repo ships an
`.envrc` with `use flake`, so the dev shell — and the `jobpipe` command — load
automatically when you `cd` in; run `direnv reload` after pulling changes to the
flake.)

### With Cargo

```sh
cargo build --release
./target/release/jobpipe --help
```

You'll need a Rust toolchain and SQLite. (TLS is via rustls, so no OpenSSL needed.)

## Quick start

```sh
# 1. Describe yourself. This file drives every score — see below.
cp profile.example.toml profile.toml
$EDITOR profile.toml

# 2. Add your Anthropic API key (only needed for `triage`).
cp .env.example .env
$EDITOR .env

# 3. Create the database and seed the company list from companies.toml.
jobpipe init

# 4. Pull postings, score them, and print the ranked digest.
jobpipe fetch
jobpipe triage --dry-run   # preview how many postings / tokens, no spend
jobpipe triage
jobpipe digest

# 5. Work the pipeline.
jobpipe apply              # interactive picker over the top postings
jobpipe track              # list open applications
jobpipe stage 12 interview --note "recruiter call went well"
jobpipe followup           # what's due for a nudge
```

> Using Nix without installing the binary? Prefix each command with `nix run . --`,
> e.g. `nix run . -- digest`. A shell alias like `alias jobpipe='nix run . --'`
> makes this painless.

## Configuration

| File | Purpose | Committed? |
| --- | --- | --- |
| `profile.toml` | **You.** Skills, experience, location, and role preferences. Passed verbatim into the scoring prompt. | No — gitignored; copy from `profile.example.toml` |
| `companies.toml` | The seed list of companies/boards to poll. | Yes — a curated starter list you can grow |
| `.env` | `ANTHROPIC_API_KEY` for triage. | No — gitignored; copy from `.env.example` |

Two sections of `profile.toml` are load-bearing, because the scoring prompt reads
their fields by name:

- **`[location]`** (`base`, `acceptable`, `work_authorization`) drives the **hard
  location gate** — a posting that doesn't match your location constraints is capped
  low no matter how good the role is. Hybrid roles are only workable when the office
  is in your `base` metro.
- **`[preferences]`** (`priority`, `also_strong`, `avoid`) drives the **rubric** —
  `priority` roles score 9–10, `also_strong` roles 7–8, `avoid` dealbreakers are
  capped low.

Everything else in the profile is free-form context for the model.

The database path defaults to `jobpipe.db` and can be overridden with `--db` or the
`JOBPIPE_DB` environment variable.

### Managing the company list

```sh
jobpipe companies add https://jobs.lever.co/acme   # auto-detects the ATS
jobpipe companies list                             # everything tracked
jobpipe companies list --needs-review              # boards that returned nothing / 404'd
```

Re-run `jobpipe init` after editing `companies.toml` to seed any new entries.

## Notes

- **Cost.** Triage uses a fast, cheap model (`claude-haiku-4-5`) — this is
  classification, not reasoning. `jobpipe triage --dry-run` prints an estimated
  cost before you spend anything, and a real run reports its actual spend from the
  API's token usage. Each posting is only ever scored once, and the system prompt is
  prompt-cached across batches to keep the bill down.
- **Re-scoring.** Changed your profile or the rubric? `jobpipe triage --retriage`
  clears existing scores on open postings and scores them again.
- **The pre-filter.** Before the LLM sees anything, a cheap code-side pass drops
  obviously-off titles to save cost. It's configured in your `profile.toml`
  `[prefilter]` section (`reject_titles` + a `keep_keywords` rescue list) — tune it
  to your search, or omit it for a conservative default. See `profile.example.toml`.

## License

MIT — see [LICENSE](LICENSE).
