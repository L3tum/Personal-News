# RSS Digest with RAG-Powered Summarization

A self-hosted daily news digest system that:
- Collects RSS feeds from **FreshRSS** (with deduplication per user)
- Stores articles in **Qdrant** (vector database) for RAG context
- Uses **Ollama** (llama.cpp) for intelligent summarization with narrative continuity
- Delivers personalized digests via **email**

## Architecture

```
FreshRSS → Unread Articles → Qdrant (vector store) → Ollama (LLM + RAG) → Email Digest
```

## Dependencies

- **Rust** (stable, with `rustfmt` and `clippy`)
- **FreshRSS** instance running and accessible
- **Qdrant** vector database
- **Ollama** with embedding model (e.g., `nomic-embed-text`) and LLM (e.g., `llama3.1`)
- SMTP server for email delivery

## Setup

1. Configure FreshRSS (see [FreshRSS API setup](#freshrss-api-setup-fever-api-required) below).
2. Copy `.env.example` to `.env` and fill in your configuration.
3. Build: `cargo build --release`
4. Run: `cargo run --release`
5. To run a single digest immediately: `cargo run --release -- --run-once`

## Docker Compose

See `docker-compose.yml` for a complete setup with FreshRSS, Qdrant, Ollama, and the digest service.

## Usage

### Running the service

```bash
# Normal mode: stays resident and runs a digest once per day at CRON_TIME in CRON_TIMEZONE (default 06:00 UTC)
cargo run --release

# Run one digest immediately and exit (great for testing or external cron)
cargo run --release -- --run-once
```

Or via Docker Compose: `docker compose up -d` (the `rss_digest` service runs the scheduler internally; `CRON_TIME` / `CRON_TIMEZONE` env vars override the schedule).

### What happens on each digest run

For **every user** in the `USERS` list, independently:

1. Fetch the user's unread articles from FreshRSS via the Fever API (articles older than the last 24 hours are skipped, and at most 1000 per run are summarized).
2. Store new articles in Qdrant and retrieve related past context (RAG) so summaries can reference earlier events.
3. Summarize each article with the Ollama LLM, plus one overall narrative paragraph.
4. If the user has a `target_language`, translate summaries via LibreTranslate.
5. Send the personalized digest email via SMTP.
6. Mark the user's articles as read in FreshRSS (with retries; a failure here is logged but does not fail the run).

Each user is isolated end-to-end: their own FreshRSS feeds, their own vector-store namespace, their own language — one user failing does not block the others.

### Multi-user setup

Each digest user must have their **own FreshRSS account** (they each see only their own feeds) and their own **API password** — see [FreshRSS API setup](#freshrss-api-setup-fever-api-required). One FreshRSS account = one personalized digest.

### Quick health checks

```bash
# Fever API auth for a user (expect "auth":1)
API_KEY=$(echo -n 'you:your_api_password_here' | md5sum | cut -d' ' -f1)
curl -s -F "api_key=$API_KEY" 'https://your-freshrss/api/fever.php?api'

# List a user's unread ids (empty string = nothing to digest)
curl -s -F "api_key=$API_KEY" 'https://your-freshrss/api/fever.php?api&unread_item_ids'
```

## Features

- **Per-user feeds**: Each user gets their own personalized digest.
- **RAG context**: The LLM retrieves related past articles and summaries to provide narrative continuity (e.g., "The war that started last week has now been stopped").
- **Deduplication**: FreshRSS deduplicates feeds per user; the vector DB avoids re-summarizing the same article.
- **Cron scheduling**: Runs daily at a configurable time.

## Important Notes

### FreshRSS API setup (Fever API, required)
The digest uses the **Fever API** (`/api/fever.php`), which authenticates **as the user** and fetches articles for that user only. FreshRSS's Fever API does **not** accept the web login password — each user needs a separate **API password**.

One-time setup (as admin):

1. **Enable the API system-wide**: FreshRSS web UI → *Administration* → *Authentication* → enable **"Allow API access (required for mobile apps)"**. (Without this, the API returns `503 Service Unavailable`.)

Per-user setup — for **every** FreshRSS account used in the `USERS` list:

2. Log in as that user → *Settings* → *Profile* → scroll to **"External access via API"** → fill in **"API password (e.g., for mobile apps)"** (≥ 7 characters) → submit.

The Fever API key is `MD5("username:api_password")`, computed by this client; it is **not** derived from the login password.

In the `USERS` array, set `freshrss_username` and `freshrss_api_password` for each user:

```json
[
  {
    "name": "You",
    "freshrss_username": "you",
    "freshrss_api_password": "your_api_password_here",
    "email": "you@example.com",
    "target_language": null
  },
  {
    "name": "Girlfriend",
    "freshrss_username": "gf",
    "freshrss_api_password": "gf_api_password",
    "email": "gf@example.com",
    "target_language": "en"
  }
]
```

If you omit `freshrss_api_password` for a user, the global `FRESHRSS_API_PASSWORD` is used — this works when all FreshRSS users share the same API password (single-user setups).

Verify a user's API access with curl:

```bash
API_KEY=$(echo -n 'you:your_api_password_here' | md5sum | cut -d' ' -f1)
curl -s -F "api_key=$API_KEY" 'https://your-freshrss/api/fever.php?api'
# expect: {"api_version":4,"auth":1,"last_refreshed_on_time":...}
```

Troubleshooting:

| Symptom | Cause |
|---|---|
| `503 Service Unavailable` | "Allow API access" is not enabled (admin → Authentication) |
| `{"api_version":4,"auth":0}` | Wrong API password, wrong username, or the user has no API password set yet (set it in Settings → Profile) |

How the client works (Fever API quirks it handles):

- `unread_item_ids` returns a comma-separated list of *all* unread ids (oldest first); the client keeps only the newest 1000 of them.
- Item details are fetched via `items&with_ids=...` in batches of 50 (the server hard-caps `items` at 50 per request).
- Feed titles/URLs come from the `feeds` action (items only carry `feed_id`).
- After a successful digest, articles are marked read in batches via `mark=item&as=read&with_ids=...`.

### FreshRSS URL Configuration
The `FRESHRSS_URL` must include the full subpath where FreshRSS is installed (e.g., `https://example.com/freshrss`), not just the domain.

### Migrating from the old greader-based setup
Older versions of this project talked to FreshRSS's Google Reader compatibility API (`/api/greader.php`) using **web login credentials**. That client has been removed; the digest now uses the **Fever API** with **per-user API passwords**. If you have an existing installation, do the following:

**1. FreshRSS side (one time, in the web UI):**

- Admin: *Administration* → *Authentication* → enable **"Allow API access (required for mobile apps)"**.
- For every user in your `USERS` list: log in as that user → *Settings* → *Profile* → set an **"API password"**. This is a **new secret you choose now** — it is not the login password, and it can be different per user.

**2. `.env` changes:**

| Old | New | Notes |
|---|---|---|
| `FRESHRSS_USERNAME=admin` | *(removed)* | No longer read. Usernames now come from the `freshrss_username` field of each `USERS` entry only (defaulting to `admin` when `USERS` is unset). |
| `FRESHRSS_PASSWORD=login_pw` | `FRESHRSS_API_PASSWORD=api_pw` | The value must now be the **API password**, not the login password. The old variable name still works as a deprecated fallback (logs a warning) so existing setups do not break immediately — remove it once migrated. |

**3. `USERS` JSON changes:**

| Old key | New key | Notes |
|---|---|---|
| `"freshrss_password"` | `"freshrss_api_password"` | Old key is still accepted (via serde alias), but switch for clarity. The value must be that user's **API password**. |
| *(none)* | `"freshrss_username"` | Unchanged, now the only source of the username. |

**4. Smoke-test before restarting:** use the [health-check curls above](#usage) for at least one user, then start the service and run once:

```bash
cargo run --release -- --run-once
```

**5. Expect this error if you skip step 1:** the run fails per user with an authentication error pointing at the *API password* (Fever API answers `"auth":0` for unknown keys, and `503` when API access is disabled system-wide). Articles are **not** marked read on failed runs, so nothing is lost — fix the API password and re-run.

No data migration is needed: Qdrant contents, SMTP settings, and Ollama configuration are unchanged.
