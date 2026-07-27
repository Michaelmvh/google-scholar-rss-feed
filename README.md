# Scholarly RSS feed generator

Generates a single RSS feed of recent scientific publications for one or more authors
**and/or journals**, sorted newest-first. Data is parsed from
[OpenAlex](https://openalex.org) (a free, open catalog of scholarly works — no API key
required).

Feeds can be defined in a config file so a feed URL never has to change, which makes this
convenient to drop into a display such as a [TRMNL](https://usetrmnl.com) via its RSS plugin.

## Deployment overview

The repository and published container are available at:

- **Source:** <https://github.com/Michaelmvh/google-scholar-rss-feed>
- **Container:** <https://github.com/Michaelmvh/google-scholar-rss-feed/pkgs/container/google-scholar-rss-feed>
- **Image:** `ghcr.io/michaelmvh/google-scholar-rss-feed:latest`

The production update path is:

```text
Edit code or feeds.toml
        |
        v
Push to GitHub main
        |
        v
GitHub Actions builds linux/amd64 and publishes to GHCR
        |
        v
The NAS pulls the new image and recreates the container
```

The application and `feeds.toml` are both baked into the image. The NAS therefore needs only
[`NAS/docker-compose.yml`](./NAS/docker-compose.yml) and a private `.env` containing the
Cloudflare token. It does not clone the source or compile Rust.

The image and Compose file update independently:

- **Code or feed changes:** push to GitHub, then pull the new GHCR image on the NAS.
- **Deployment changes:** upload the changed `NAS/docker-compose.yml` to the NAS and recreate
  the Container Manager project. GHCR cannot update the Compose file itself.

## Feed configuration

Named feeds live in [`feeds.toml`](./feeds.toml), keeping the public URL stable when authors,
journals, or topics change.

Before production use, replace the placeholder `mailto` with a real contact address. OpenAlex
uses it for its polite API pool; it is sent to OpenAlex but is not included in the RSS feed.

```toml
default_feed = "myfield"           # served at bare "/"

[settings]
mailto = "you@example.com"         # replace with a real contact address
from_days = 365                    # default recency window

[feeds.myfield]
title = "Machine Learning & Synthetic Biology"
author_ids = ["A5135542215", "A5005023517", "A5010124873"]

[feeds.synbio]
title = "Synthetic Biology"
author_ids = ["A5135542215", "A5005023517"]

# Journal-only feeds are also supported.
[feeds.top-journals]
title = "Top Journals"
source_ids = ["S137773608", "S64187185"]
```

Per-feed keys:

| Key | Purpose |
|---|---|
| `title` | Optional RSS channel title |
| `author_ids` | OpenAlex author IDs; preferred because they are the most precise |
| `orcids` | ORCIDs resolved to OpenAlex author IDs |
| `authors` | Author names resolved by top search result; imprecise for common names |
| `source_ids` | OpenAlex journal/source IDs; preferred |
| `issns` | ISSNs resolved to OpenAlex source IDs |
| `journals` | Journal names resolved by top search result |
| `topics` | OpenAlex topic IDs used to narrow results |
| `from` | Explicit earliest publication date in `YYYY-MM-DD` format |

A feed requires at least one author or journal. If both are present, results are the **union**
of the authors' publications and recent publications in the journals. Results are
de-duplicated and sorted newest-first. A topic filter applies to both sides of the union.

The config is read on every request. Identical resolved queries are cached for up to one hour,
so restarting the application clears the cache immediately. In Docker, changing the repository
copy of `feeds.toml` requires publishing and deploying a new image because the file is baked
into that image.

### Finding reliable OpenAlex IDs

Prefer IDs over names because author profiles can be conflated or fragmented:

```text
https://api.openalex.org/authors?search=Jeff%20Nivala
https://api.openalex.org/sources?search=Nature%20Communications
https://api.openalex.org/topics?search=synthetic%20biology
```

OpenAlex URLs and bare IDs are both accepted and normalized, for example
`https://openalex.org/A5005023517` and `A5005023517`.

## Feed URLs

After starting the service:

- Default configured feed: `http://localhost:3005/`
- Named feed: `http://localhost:3005/?feed=myfield`
- Multiple ad-hoc authors:
  `http://localhost:3005/?author_id=A5135542215&author_id=A5005023517`

All identifier parameters are repeatable and are merged with a selected named feed:

| Parameter | Description |
|---|---|
| `feed` | Name under `[feeds.<name>]` in `feeds.toml` |
| `author_id` | OpenAlex author ID |
| `orcid` | ORCID resolved to an OpenAlex author |
| `author` | Author name resolved using the top search result |
| `source_id` | OpenAlex journal/source ID |
| `issn` | ISSN resolved to an OpenAlex source |
| `journal` | Journal name resolved using the top search result |
| `topic` | OpenAlex topic ID used to constrain results |
| `concept` | Alias for `topic` |
| `from` | Earliest publication date, `YYYY-MM-DD` |

The server returns RSS XML with CORS enabled. The generated channel has a 60-minute TTL, and
the in-memory cache is cleared hourly.

## Running locally

### Native Rust

Install Rust with `rustup`, then run:

```sh
cargo run
# http://127.0.0.1:3005/?feed=myfield
```

The optional positional argument changes the bind address, and `--config` changes the config
path:

```sh
cargo run -- "0.0.0.0:3005" --config feeds.toml
GSRF_CONFIG=feeds.toml cargo run
```

Useful development checks:

```sh
cargo test
cargo clippy --all-targets --all-features
```

### Local Docker Compose

[`local/docker-compose.yml`](./local/docker-compose.yml) publishes port `3005` and can either
pull the GHCR image or build the current checkout.

Pull and run the published image:

```sh
docker compose -f local/docker-compose.yml pull
docker compose -f local/docker-compose.yml up -d
```

Build and run the current source:

```sh
docker compose -f local/docker-compose.yml up -d --build
```

Inspect or stop it:

```sh
docker compose -f local/docker-compose.yml logs -f
docker compose -f local/docker-compose.yml down
```

The local Compose file contains a commented bind mount for `feeds.toml`. Enable it when config
changes should be visible without rebuilding the image; restart the container to clear any
cached feed immediately.

## GitHub Actions and GHCR

The [`Publish Docker image`](./.github/workflows/docker-publish.yml) workflow runs when:

- A commit is pushed to `main`
- A tag beginning with `v` is pushed
- It is manually started from **GitHub → Actions → Publish Docker image → Run workflow**

It uses the repository's built-in `GITHUB_TOKEN`; no registry secret is required. Published
tags include `latest` for `main`, branch/semantic-version tags where applicable, and an
immutable `sha-...` tag for each build. The image targets `linux/amd64`, matching the Synology
DS423+.

After pushing a change, verify the workflow succeeded in the repository's **Actions** tab
before expecting the NAS to find a new image.

### Package visibility

For anonymous NAS pulls, set the package to public once:

**GitHub profile → Packages → google-scholar-rss-feed → Package settings → Change visibility
→ Public**

To keep it private instead, authenticate the NAS using a GitHub personal access token with
`read:packages`:

```sh
echo "YOUR_PAT" | /usr/local/bin/docker login ghcr.io \
  --username Michaelmvh --password-stdin
```

## Synology NAS and Cloudflare Tunnel

The production file is [`NAS/docker-compose.yml`](./NAS/docker-compose.yml). It runs:

- `scholar-rss`: the prebuilt GHCR application image, available only to the Compose network.
- `scholar-rss-tunnel`: Cloudflare's connector, which provides public HTTPS access.

No router port forwarding is needed, and port `3005` is not published on the NAS.

### Prerequisites

- Synology Container Manager installed on the DS423+.
- The GHCR package is public, or the NAS is authenticated to GHCR.
- `michaelmvh.com` is active in Cloudflare DNS.
- A Cloudflare Zero Trust tunnel and token.

### GitHub Pages DNS safety

The existing GitHub Pages site can coexist with the tunnel:

- Keep the four GitHub Pages `A` records and the `www` CNAME set to **DNS only** (grey cloud).
- Keep mail records such as MX, SPF, DKIM, and domain-verification records DNS only.
- Only the tunnel hostname, `reading.michaelmvh.com`, should be proxied by Cloudflare.
- The tunnel hostname is independent of the apex and `www` records used by GitHub Pages.

### First-time deployment

1. In **Cloudflare Zero Trust → Networks → Tunnels**, create or open the tunnel.
2. Add a published application/public hostname:
   - Hostname: `reading.michaelmvh.com`
   - Service type: `HTTP`
   - Service URL: `scholar-rss:3005`
3. Copy only the tunnel token from Cloudflare's installation command. Do not run that command;
   the Compose file already runs `cloudflared`.
4. In Synology File Station, create:

   ```text
   /volume1/docker/scholar-rss/
   ```

5. Upload [`NAS/docker-compose.yml`](./NAS/docker-compose.yml) directly into that directory.
   No rename is required.
6. Create a plain-text file named `.env` beside it:

   ```env
   TUNNEL_TOKEN=eyJ...
   ```

   The resulting directory must contain:

   ```text
   /volume1/docker/scholar-rss/
   ├── docker-compose.yml
   └── .env
   ```

   `.env` may be hidden in File Station. Enable **Settings → General → Show hidden files** if
   necessary.
7. Open **Container Manager → Project → Create**:
   - Project name: `scholar-rss`
   - Path: `/volume1/docker/scholar-rss`
   - Source: existing `docker-compose.yml`
8. Build/start the project and confirm both `scholar-rss` and `scholar-rss-tunnel` are running.
9. Open:

   ```text
   https://reading.michaelmvh.com/?feed=myfield
   ```

10. Configure the TRMNL RSS plugin with that HTTPS URL. The URL remains stable when the named
    feed's contents change.

### Automatic image updates

Container Manager does not automatically redeploy a changed `latest` image. To automate it,
open **DSM Control Panel → Task Scheduler → Create → Scheduled Task → User-defined script**:

- Run as: `root`
- Schedule: daily at a convenient time
- Script:

```sh
cd /volume1/docker/scholar-rss &&
/usr/local/bin/docker compose pull &&
/usr/local/bin/docker compose up -d --remove-orphans
```

This updates both the application and `cloudflared`. It leaves the existing containers running
if pulling an image fails because the commands are chained with `&&`.

For a manual update, run the same command over SSH or use **Pull** and rebuild the project in
Container Manager.

### Updating the Compose configuration

The scheduled task updates images, not `docker-compose.yml`. If
[`NAS/docker-compose.yml`](./NAS/docker-compose.yml) changes:

1. Download or upload the new file from GitHub over the existing NAS copy.
2. Keep the existing `.env`; it contains the tunnel secret.
3. Rebuild/recreate the Container Manager project.

### Rollback

Every successful workflow build publishes an immutable `sha-...` image tag. To roll back:

1. Find the desired tag on the GitHub package page.
2. Change the NAS Compose image temporarily:

   ```yaml
   image: ghcr.io/michaelmvh/google-scholar-rss-feed:sha-abcdef0
   ```

3. Pull and recreate the project.
4. Change it back to `:latest` after the problem is fixed.

## Caching and request load

Normal TRMNL polling will not overwhelm the NAS. The generated RSS channel for each resolved
query is cached in memory for up to one hour. A cache hit only clones and serializes the
already-built channel; it does not call OpenAlex again. A cold request makes at most one author
works query and one journal works query, concurrently.

Use stable OpenAlex IDs in `feeds.toml` and give TRMNL a named feed URL such as
`https://reading.michaelmvh.com/?feed=myfield`. Name, ORCID, ISSN, and journal-name parameters
must be resolved before the channel-cache lookup and can therefore cause extra OpenAlex calls.

Current limitations:

- Cloudflare Tunnel transports requests but does not automatically cache this dynamic RSS URL.
- The application has no rate limiter.
- Different ad-hoc parameters create different cache entries until the hourly cache clear.
- Simultaneous requests for the same uncached feed can each start an OpenAlex fetch before the
  first result enters the cache.

These limitations are not significant for ordinary TRMNL use, but Cloudflare can cheaply
protect the public endpoint:

1. In Cloudflare, open **Caching → Cache Rules → Create rule**.
2. Name the rule `Cache reading RSS feed`.
3. Use the expression:

   ```text
   (http.host eq "reading.michaelmvh.com")
   ```

4. Set **Cache eligibility** to **Eligible for cache**.
5. Set an **Edge TTL** of two hours. The application sends
   `Cache-Control: public, max-age=300, s-maxage=7200, stale-while-revalidate=86400` so browsers
   may cache for five minutes and shared caches may retain a response for two hours.
6. Keep query strings in the cache key so different named feeds remain separate.

Cloudflare's first request for a URL is a cache miss and reaches the NAS; subsequent requests
for that exact URL are served at the edge until expiration. Verify the rule by requesting the
same URL twice and inspecting `CF-Cache-Status`:

```sh
curl -sI "https://reading.michaelmvh.com/?feed=myfield" | grep -i cf-cache-status
curl -sI "https://reading.michaelmvh.com/?feed=myfield" | grep -i cf-cache-status
```

The first response should normally be `MISS` and a later response `HIT`. `DYNAMIC` or `BYPASS`
means the Cache Rule is not applying.

After deploying a feed change that must appear immediately, use **Cloudflare → Caching →
Configuration → Purge Cache → Custom Purge** for the feed URL. Otherwise, allow up to the edge
TTL for the cached response to expire.

Edge caching protects repeated URLs, but unique query strings can still force cache misses.
As an additional abuse safeguard, create a Cloudflare rate-limiting rule:

1. Open **Security → WAF → Rate limiting rules**.
2. Match hostname `reading.michaelmvh.com`.
3. Set a limit such as 60 requests per minute per client IP.
4. Block or challenge the client briefly after exceeding the limit.

This threshold is far above normal RSS polling while preventing one client from continuously
generating unique ad-hoc queries. Availability and exact limits depend on the Cloudflare plan.
Do not place Cloudflare Access login in front of the feed unless the TRMNL client is configured
to supply the required credentials.

## Security and recovery

- Never commit `.env` or the Cloudflare tunnel token. `.env` is ignored by Git.
- If the token is exposed, rotate it in Cloudflare and replace the NAS `.env` value.
- The public RSS endpoint has no application authentication. Do not put private information in
  feed titles or configuration.
- Do not open or forward port `3005` on the router; the tunnel is the public entry point.
- Repository code, feed definitions, Compose configuration, and image history are stored on
  GitHub/GHCR. The only NAS-specific secret to preserve is `.env`.
- To recover on a replacement NAS, install Container Manager, upload
  `NAS/docker-compose.yml`, create `.env` with a valid tunnel token, and create the project.

## Troubleshooting

| Symptom | Resolution |
|---|---|
| GHCR pull returns `unauthorized` or `denied` | Make the package public or run `docker login ghcr.io` with a token containing `read:packages`. |
| Compose reports `TUNNEL_TOKEN` is unset | Confirm `.env` is beside `docker-compose.yml`, is named exactly `.env`, and contains `TUNNEL_TOKEN=value` without spaces around `=`. |
| Tunnel shows disconnected | Check the `scholar-rss-tunnel` logs and confirm its token is current. |
| Cloudflare returns HTTP 502 | Confirm `scholar-rss` is running and the Cloudflare service URL is exactly `http://scholar-rss:3005`, not `localhost`. |
| Public hostname has a certificate warning during DNS setup | Wait until the domain is active. GitHub Pages records should remain DNS only; the tunnel hostname should be proxied. |
| Feed content appears stale | Feeds are cached for up to one hour. Restart `scholar-rss` to clear the cache immediately. |
| A pushed feed change does not appear | Confirm GitHub Actions succeeded, then confirm the NAS pulled and recreated the new `latest` image. |
| A local `feeds.toml` change does not appear in Docker | Rebuild the image, or enable the bind mount in `local/docker-compose.yml` and restart the container. |
| An author returns unrelated papers | Replace name lookup with a precise OpenAlex author ID or ORCID, and optionally add topic IDs. |
