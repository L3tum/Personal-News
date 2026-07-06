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

### FreshRSS API Endpoint Configuration
The digest uses the **Google Reader compatibility API** (`/api/greader.php`), which is the only API currently available in modern FreshRSS installations. The native `g.php` API is no longer used.

Verify the API works with curl:
```bash
curl -u admin:password "https://your-freshrss/api/greader.php" -d "cmd=reader/api/0/stream/contents&stream=feed/"
```

If you get authentication errors, see below.

### FreshRSS URL Configuration
The `FRESHRSS_URL` must include the full subpath where FreshRSS is installed (e.g., `https://example.com/freshrss/`), not just the domain.

### Authentication (optional)
The digest uses **basic authentication** (username/password) for the FreshRSS API. If you've disabled authentication in FreshRSS:
- The code will still send the credentials, but FreshRSS will ignore them if auth is disabled.
- Alternatively, you can modify `src/freshrss.rs` to remove the `.basic_auth()` call if your FreshRSS has no auth at all.

If password authentication is required but disabled for a user:
1. Go to FreshRSS → **Administration** → **Access management**
2. Ensure the admin user's "Password" auth method is allowed.
