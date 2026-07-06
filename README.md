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

1. Copy `.env.example` to `.env` and fill in your configuration.
2. Build: `cargo build --release`
3. Run: `cargo run --release`
4. To run a single digest immediately: `cargo run --release -- --run-once`

## Docker Compose

See `docker-compose.yml` for a complete setup with FreshRSS, Qdrant, Ollama, and the digest service.

## Features

- **Per-user feeds**: Each user gets their own personalized digest.
- **RAG context**: The LLM retrieves related past articles and summaries to provide narrative continuity (e.g., "The war that started last week has now been stopped").
- **Deduplication**: FreshRSS deduplicates feeds per user; the vector DB avoids re-summarizing the same article.
- **Cron scheduling**: Runs daily at a configurable time.

## Important Notes

### Per-User FreshRSS Authentication (Required for Multi-User)
The digest uses the **Google Reader compatibility API** (`/api/greader.php`), which authenticates **as the user** and fetches articles for that user only. There is no `user=` parameter like the old `g.php` API — the API operates on the authenticated user.

Therefore, **each digest user must have their own FreshRSS account with a password**. In the `USERS` array, set `freshrss_username` and `freshrss_password` for each user:

```json
[
  {
    "name": "You",
    "freshrss_username": "you",
    "freshrss_password": "your_password_here",
    "email": "you@example.com",
    "target_language": null
  },
  {
    "name": "Girlfriend",
    "freshrss_username": "gf",
    "freshrss_password": "gf_password",
    "email": "gf@example.com",
    "target_language": "en"
  }
]
```

If you omit `freshrss_password` for a user, the global `FRESHRSS_USERNAME`/`PASSWORD` will be used (useful for a single-user setup).

Verify a user's API access with curl:
```bash
curl -u you:your_password_here "https://your-freshrss/api/greader.php" -d "cmd=reader/api/0/stream/contents&stream=feed/"
```

If you get authentication errors, ensure the user has a FreshRSS password set (Administration → Access management → Password).

### FreshRSS URL Configuration
The `FRESHRSS_URL` must include the full subpath where FreshRSS is installed (e.g., `https://example.com/freshrss/`), not just the domain.
