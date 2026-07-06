# Fix FreshRSS 404 API Error

## Problem

The digest is failing with a 404 error when calling the FreshRSS API:

```
FreshRSS API error: 404 Not Found - <!DOCTYPE HTML PUBLIC "-//W3C//DTD HTML 4.01//EN" ...
```

The API call in `freshrss.rs` constructs URLs like:
```
{FRESHRSS_URL}/api/g.php?get=entries&user={}&feeds=-1&state=_notread&since={}&order=desc&sort=date&export=flatjson
```

The 404 with a generic Apache/Nginx HTML response means the endpoint `/api/g.php` does not exist at the configured `FRESHRSS_URL`.

## Root Cause

The current code (`src/freshrss.rs`) uses the FreshRSS API endpoint `/api/g.php`, which appears to no longer exist or is not available in your FreshRSS instance. The only working API is the **Google Reader compatibility API** at `/api/greader.php`.

The code needs to be updated to use `greader.php` instead of `g.php`.

## Fix — Rewrite `src/freshrss.rs` to use `greader.php`

Replace the entire FreshRSS API client in `src/freshrss.rs` to use the Google Reader compatibility API (`greader.php`) instead of `g.php`.

### Changes needed in `src/freshrss.rs`:

1. **`fetch_unread_articles()`**: Change from GET request to `g.php` → POST request to `greader.php` with `application/x-www-form-urlencoded` body containing `cmd=reader/api/0/stream/contents&stream=feed/` (for all unread entries). Parse the different JSON response format (array of items with keys like `id`, `title`, `canonical`, etc.).

2. **`mark_as_read()`**: Change from POST to `g.php` → POST to `greader.php` with `cmd=reader/api/0/edit/mark-as-read&i=<entry-id>`.

3. **Response parsing**: The `greader.php` API returns items as JSON objects with `id`, `title`, `canonical`, `summary`, `streamIds`, `ts` (Unix timestamp), etc. — different from the current response format.

### Reference
See FreshRSS source: https://github.com/FreshRSS/FreshRSS/blob/edge/p/api/greader.php

## Files modified

- `src/freshrss.rs` — rewritten to use Google Reader API (`greader.php`) instead of non-existent `g.php`
- `README.md` — updated to reflect the `greader.php` API

## Verification

1. The code compiles cleanly with no errors
2. Test the `greader.php` API with curl before deploying:
   ```bash
   curl -u admin:yourpassword "https://your-freshrss/api/greader.php" -d "cmd=reader/api/0/stream/contents&stream=feed/"
   ```
3. Run the digest with `--run-once` and verify it fetches articles successfully
