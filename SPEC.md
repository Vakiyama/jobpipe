# jobpipe — project spec

A Rust CLI that replaces manual LinkedIn browsing with a daily ranked digest of new job
postings, pulled directly from company ATS boards.

This document is the build brief. Read it fully before writing code. Ask before deviating
from the non-goals or the module boundaries.

---

## 1. Problem

The operator is a full-stack engineer doing an active job search. Current process: manually
browse LinkedIn daily, eyeball postings for fit, apply. This costs ~1–2 hours/day and has
poor recall — good roles at smaller companies never surface.

The bottleneck is **discovery and triage**, not application volume. The operator already
converts applications to interviews at a healthy rate; automating submission would degrade
that. Do not build an auto-applier.

Target end state: one command (or one cron job) produces a ranked list of 5–15 new,
plausibly-relevant postings each morning, with apply links. The operator reads it in 10
minutes and applies by hand.

---

## 2. Non-goals

Explicitly out of scope. Do not build these, and do not suggest them:

- **No LinkedIn / Indeed / Glassdoor scraping.** These have aggressive anti-bot systems and
  ToS enforcement. The operator's LinkedIn account is load-bearing for the job search; risking
  a suspension is unacceptable. Source data from public ATS APIs only.
- **No automated form submission.** No Playwright, no Selenium, no headless browser anywhere
  in this project. Every application is submitted by a human.
- **No cover letter generation.** The operator does not use cover letters and this has not
  hurt results.
- **No proxies, no rotating user agents, no rate-limit evasion.** Every endpoint used here is
  a public, unauthenticated JSON API intended for browser access. If something requires
  evasion to fetch, drop that source instead.
- **No web UI.** CLI + a generated report file. Keep it small.

---

## 3. Stack

- Rust, 2021 edition.
- `tokio` — async runtime, concurrent board fetching.
- `reqwest` (json, rustls-tls) — HTTP.
- `serde` / `serde_json` — deserialization. ATS schemas differ wildly; normalize at the edge.
- `sea-orm` + `sea-orm-migration` with SQLite — persistence and dedup. Use entity structs and
  the migration DSL; the SQL in section 6 is the target schema, not the interface. Do not
  substitute sqlx or diesel — this is a deliberate choice, the operator uses SeaORM elsewhere
  and wants consistency across projects.
- `clap` (derive) — CLI.
- `anyhow` for application errors, `thiserror` for the fetcher layer.
- `tracing` + `tracing-subscriber` — logging.
- `chrono` — timestamps.

Concurrency target: fetch all boards with a bounded concurrency of ~10 simultaneous requests.
A 300-board run should complete in well under a minute.

---

## 4. Data sources

### 4.1 ATS boards (primary)

All are public, unauthenticated, no API key. One request per company returns every open role.
There is **no cross-company search endpoint** — each board is fetched independently by slug.

| ATS | Endpoint |
|---|---|
| Greenhouse | `https://boards-api.greenhouse.io/v1/boards/{slug}/jobs?content=true` |
| Lever | `https://api.lever.co/v0/postings/{slug}?mode=json` |
| Ashby | `https://api.ashbyhq.com/posting-api/job-board/{slug}` |
| Workable | `https://apply.workable.com/api/v1/widget/accounts/{slug}?details=true` |
| Recruitee | `https://{slug}.recruitee.com/api/offers/` |
| SmartRecruiters | `https://api.smartrecruiters.com/v1/companies/{slug}/postings` |

Verify each shape against a live response before writing the deserializer — do not trust the
field names in this document. Greenhouse returns HTML-escaped job content; unescape it.
Ashby wraps results in a `jobs` array. Lever returns a bare array.

A board returning an empty result or 404 usually means the company changed ATS. Log it, mark
the company `needs_review` in the DB, and continue — a failed board must never abort the run.

### 4.2 Secondary sources (phase 3, optional)

- HN "Who is Hiring" via Algolia: `https://hn.algolia.com/api/v1/search?tags=story&query=Ask HN: Who is hiring`
  then fetch the thread's comments. Disproportionately good for Rust roles.
- RemoteOK: `https://remoteok.com/api`
- Remotive: `https://remotive.com/api/remote-jobs`

Treat these as a separate `Source` impl behind the same trait. Do not block phase 1 on them.

---

## 5. Architecture

```
src/
  main.rs           CLI entry, wiring
  cli.rs            clap definitions
  config.rs         profile + settings loading (TOML)
  db/
    mod.rs          connection setup, migration runner
    entities/       SeaORM entity structs (company.rs, posting.rs, application.rs, prelude.rs)
    queries.rs      higher-level query helpers built on the entities
  migration/        sea-orm-migration crate (m0001_init.rs, ...)
  sources/
    mod.rs          `trait JobSource { async fn fetch(&self, slug: &str) -> Result<Vec<RawPosting>> }`
    greenhouse.rs
    lever.rs
    ashby.rs
    workable.rs
    ...
  normalize.rs      RawPosting -> Posting; strip HTML, canonicalize location/remote flags
  triage.rs         LLM scoring
  report.rs         digest rendering (markdown + terminal)
  followup.rs       nag logic over the applications table
```

The `JobSource` trait is the key seam. Adding an ATS should mean one new file and one line in
a registry — nothing else changes.

---

## 6. Schema

```sql
CREATE TABLE companies (
  id            INTEGER PRIMARY KEY,
  name          TEXT NOT NULL,
  ats           TEXT NOT NULL,          -- greenhouse | lever | ashby | ...
  slug          TEXT NOT NULL,
  careers_url   TEXT,
  tags          TEXT,                   -- csv freeform labels: backend, remote, fintech, ...
  active        INTEGER NOT NULL DEFAULT 1,
  needs_review  INTEGER NOT NULL DEFAULT 0,
  last_fetched  TEXT,
  UNIQUE(ats, slug)
);

CREATE TABLE postings (
  id            INTEGER PRIMARY KEY,
  company_id    INTEGER NOT NULL REFERENCES companies(id),
  external_id   TEXT NOT NULL,          -- ATS-native id
  title         TEXT NOT NULL,
  location      TEXT,
  remote        TEXT,                   -- remote | hybrid | onsite | unknown
  description   TEXT NOT NULL,
  apply_url     TEXT NOT NULL,
  first_seen    TEXT NOT NULL,
  last_seen     TEXT NOT NULL,
  closed_at     TEXT,
  score         INTEGER,                -- 0-10, null until triaged
  score_reason  TEXT,
  flags         TEXT,                   -- json array
  triaged_at    TEXT,
  UNIQUE(company_id, external_id)
);

CREATE TABLE applications (
  id            INTEGER PRIMARY KEY,
  posting_id    INTEGER NOT NULL REFERENCES postings(id),
  applied_at    TEXT NOT NULL,
  stage         TEXT NOT NULL DEFAULT 'applied',  -- applied | screen | interview | offer | rejected | ghosted
  last_contact  TEXT,
  next_followup TEXT,
  notes         TEXT
);
```

Dedup is `UNIQUE(company_id, external_id)`. On each run, upsert and bump `last_seen`; a
posting whose `last_seen` falls more than 3 days behind the run timestamp gets `closed_at` set.
Only rows where `triaged_at IS NULL` go to the LLM — never re-score a posting.

---

## 7. Triage

The scoring step is the only paid component. Batch 10–20 postings per request to keep cost
down; at ~50 new postings/day this should cost single-digit cents.

Use the Anthropic Messages API. Model: a fast/cheap tier is correct here — this is
classification, not reasoning. Read the API key from `ANTHROPIC_API_KEY`.

System prompt should establish: you are screening job postings against one candidate's
profile; output JSON only, no prose, no markdown fences.

Per posting, return:

```json
{
  "external_id": "...",
  "score": 0,
  "reason": "one sentence, max 20 words",
  "flags": ["requires_citizenship", "senior_only", "priority_stack", "no_location", "contract"]
}
```

A hard location gate is applied first — see the profile's `[location]` block. If none of a
posting's locations are workable, the score is capped at 3 regardless of stack fit.

Scoring rubric to embed in the prompt, judged against the profile's `[preferences]` (`priority`
= highest-value target, `also_strong` = a genuine second specialty, `avoid` = dealbreakers),
`[skills]`, and seniority (`seniority_band`, `years_experience`):

- **9–10** — A role squarely in the candidate's `priority` area at or near their experience
  level, or one that explicitly names their `priority` stack.
- **7–8** — A strong match on the candidate's core `[skills]` within their seniority band, or a
  role in their `also_strong` secondary specialty.
- **4–6** — Plausible but mismatched on one axis (slightly senior, adjacent stack, unclear location).
- **0–3** — Wrong discipline, wrong seniority by 5+ years, hits an `avoid` dealbreaker the
  candidate cannot satisfy, or location failed the gate.

Hard filters applied in code *before* the LLM call, to save spend: drop postings whose title
matches the configured obvious-reject regex (`[prefilter].reject_titles`) unless the description
mentions a keep-keyword (`[prefilter].keep_keywords`, typically the candidate's priority stack).

Digest threshold: show score ≥ 7 by default, `--min-score` to override.

### 7.1 Candidate profile

The candidate profile lives in `profile.toml`; see `profile.example.toml` for the full schema
and per-field documentation. `profile.toml` is gitignored — only the example is committed.

The entire file is passed verbatim into the triage prompt, so the model scores postings against
exactly what it contains. The structure is free-form except for three load-bearing sections
whose field names the scoring prompt references directly:

- `[location]` — `base`, `acceptable`, `work_authorization` drive the hard location gate.
- `[preferences]` — `priority`, `also_strong`, `avoid` drive the scoring rubric (section 7).
- `[prefilter]` (optional) — `reject_titles`, `keep_keywords` configure the code-side pre-filter.

Everything else (`[candidate]`, `[skills]`, `[experience.*]`, `[projects.*]`) is free-form
context that helps the model judge fit.

---

## 8. CLI

```
jobpipe init                        # create db, run migrations, seed companies from companies.toml
jobpipe companies add <url>         # detect ATS from a careers URL, insert
jobpipe companies list [--needs-review]
jobpipe fetch [--only <ats>]        # poll all active boards, upsert postings
jobpipe triage [--limit N]          # score untriaged postings
jobpipe digest [--min-score 7] [--since 1d] [--format md|term]
jobpipe run                         # fetch + triage + digest, for cron
jobpipe apply <posting_id>          # record an application, open apply_url
jobpipe track [--stage ...]         # list open applications
jobpipe followup                    # list applications needing a nudge
jobpipe stage <application_id> <stage> [--note "..."]
```

`companies add <url>` should sniff the ATS from the URL pattern
(`boards.greenhouse.io/acme` → greenhouse/acme, `jobs.lever.co/acme` → lever/acme,
`jobs.ashbyhq.com/acme` → ashby/acme) and fall back to fetching the page and looking for
known embed markers.

---

## 9. Follow-up rules

The tracker exists because applications get dropped. Default nag schedule:

- 7 days after `applied` with no contact → suggest a follow-up email.
- 21 days after `applied` with no contact → suggest marking `ghosted`.
- 5 days after any interview stage with no contact → suggest a check-in.

`jobpipe followup` prints these. No email sending — just the reminder.

---

## 10. Build order

Ship each phase working before starting the next. Do not scaffold all of it up front.

1. **Phase 1 — one source, end to end.** Greenhouse only. `init`, `fetch`, and a dumb
   `digest` that prints everything new. Seed with 10 hand-picked companies. Prove the loop.
2. **Phase 2 — triage.** Add the LLM scoring pass, the pre-filter regex, `profile.toml`,
   and score-thresholded digest output. This is where it becomes actually useful.
3. **Phase 3 — breadth.** Lever, Ashby, Workable, Recruitee. Expand the company list to
   150–400. Add `companies add` with ATS sniffing.
4. **Phase 4 — tracking.** `apply`, `track`, `stage`, `followup`.
5. **Phase 5 — polish.** Secondary sources (HN/RemoteOK), `run` for cron, markdown report
   written to a file.

---

## 11. Notes and known pitfalls

- **Ghost jobs.** Roughly a fifth of postings are never meant to be filled. Sourcing from
  company ATS boards rather than aggregators reduces this but does not eliminate it. Do not
  build features that assume a posting is real.
- **Reposts.** Companies close and reopen the same role with a new `external_id`. Consider a
  soft-dedup on `(company_id, normalized_title)` for the digest so the same role doesn't
  resurface weekly.
- **Slug drift.** The company list decays. `needs_review` plus a periodic `companies list
  --needs-review` is the maintenance loop.
- **Rate limiting.** Be polite: bounded concurrency, a real User-Agent identifying the tool,
  and don't poll more than a few times a day. These endpoints are free and public; keep them
  that way.
- **Cost ceiling.** Add a `--dry-run` to `triage` that reports how many postings would be sent
  and an estimated token count before spending anything.

---

## 12. Definition of done for phase 1

`cargo run -- init && cargo run -- fetch && cargo run -- digest` against 10 seeded Greenhouse
companies prints a list of open roles with titles, locations, and apply URLs, and a second
`fetch` run adds zero duplicates.
