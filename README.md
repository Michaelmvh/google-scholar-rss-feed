# Scholarly RSS feed generator

Generates a single RSS feed of recent scientific publications for one or more authors
**and/or journals**, sorted newest-first. Data is parsed from
[OpenAlex](https://openalex.org) (a free, open catalog of scholarly works — no API key
required).

Feeds can be defined in a config file so a feed URL never has to change, which makes this
convenient to drop into a display such as a [TRMNL](https://usetrmnl.com) via its RSS plugin.

## How to use

1. Launch the binary (optionally passing a bind address and a config file):
   ```sh
   cargo run                                   # 127.0.0.1:3005, config ./feeds.toml
   cargo run "0.0.0.0:3005" --config feeds.toml
   ```
   The config path can also be set via the `GSRF_CONFIG` environment variable. Default port
   is 3005.

2. Request a feed:
   - **Configured feed:** `http://localhost:3005/?feed=myfield`
   - **Default feed** (the `default_feed` from the config): `http://localhost:3005/`
   - **Ad-hoc, by OpenAlex author id:**
     `http://localhost:3005/?author_id=A5135542215&author_id=A5005023517`

## Config file (`feeds.toml`)

Define named feeds so you don't have to keep editing the URL. See the included
[`feeds.toml`](./feeds.toml) for a full example.

```toml
default_feed = "myfield"          # feed served at bare "/"

[settings]
mailto = "you@example.com"         # OpenAlex "polite pool" contact
from_days = 365                    # default recency window when a feed omits `from`

[feeds.myfield]
title = "Machine Learning & Synthetic Biology"
author_ids = ["A5135542215", "A5005023517", "A5010124873"]

[feeds.synbio]
title = "Synthetic Biology"
author_ids = ["A5135542215", "A5005023517"]

# Journals (sources) can be included too — this feed is journal-only.
[feeds.top-journals]
title = "Top Journals"
source_ids = ["S137773608", "S64187185"]  # Nature, Nature Communications
```

Per-feed keys:
- Authors: `author_ids`, `orcids`, `authors` (names).
- Journals: `source_ids`, `issns`, `journals` (names).
- Other: `title`, `topics` (OpenAlex topic ids), `from` (`YYYY-MM-DD`, overrides
  `from_days`).

A feed needs at least one author **or** journal. When a feed lists **both** authors and
journals, the result is the **union**: the authors' papers **plus** all recent papers in
the journals (merged, de-duplicated, and date-sorted). A feed's `topics` filter, when set,
also narrows the journal side so a high-volume journal doesn't drown out author papers.

The config file is re-read on every request, so edits take effect **without restarting**
the server. If the file is missing, the server still works using ad-hoc URL parameters.

## URL parameters

All identifier parameters are repeatable and are merged with the selected feed (if any):

| Param        | Description                                                              |
|--------------|--------------------------------------------------------------------------|
| `feed`       | Name of a feed defined in the config file.                               |
| `author_id`  | OpenAlex author id (e.g. `A5005023517`) — most precise.                  |
| `orcid`      | ORCID; resolved to an OpenAlex author id.                                |
| `author`     | Author name; resolved via search (top match). Imprecise for common names.|
| `source_id`  | OpenAlex source (journal) id (e.g. `S137773608`) — most precise.         |
| `issn`       | Journal ISSN; resolved to an OpenAlex source id.                         |
| `journal`    | Journal name; resolved via search (top match). Imprecise for common names.|
| `topic`      | OpenAlex topic id to constrain results (helps disambiguate common names). `concept` is accepted as an alias. |
| `from`       | Earliest publication date, `YYYY-MM-DD` (defaults to `from_days`).       |

Providing both author and journal identifiers yields the **union** (authors' papers plus
the journals' papers).

### Finding author and journal ids

Search OpenAlex to get a stable id (recommended over names, which OpenAlex may conflate or
fragment):

```
https://api.openalex.org/authors?search=Jeff%20Nivala
https://api.openalex.org/sources?search=Nature%20Communications
```

## TRMNL

Point the TRMNL **RSS** plugin at a configured feed URL, e.g.
`http://<your-host>:3005/?feed=myfield`. Because the feed is defined in `feeds.toml`, you
can add or change authors and journals by editing the file — the TRMNL URL never changes.

## Running with Docker

A multi-stage [`Dockerfile`](./Dockerfile) and [`docker-compose.yml`](./docker-compose.yml)
are included. The image binds to `0.0.0.0:3005` and expects `feeds.toml` mounted at
`/config/feeds.toml` (already wired up in the compose file). No CA-certificate package is
needed — TLS to the OpenAlex API uses `rustls`' bundled roots.

The compose file references a **prebuilt image** published to the GitHub Container Registry
(GHCR) by [`.github/workflows/docker-publish.yml`](./.github/workflows/docker-publish.yml),
so the NAS never has to compile anything:

```sh
docker compose pull        # fetch the prebuilt ghcr.io image
docker compose up -d
# then browse http://<host>:3005/?feed=myfield
```

For local development you can still build from source instead of pulling:

```sh
docker compose up -d --build
```

Edit `feeds.toml` on the host at any time; it is re-read on every request, so changes take
effect without restarting the container.

### Publishing the image (one-time setup)

The workflow builds `linux/amd64` and pushes to
`ghcr.io/michaelmvh/google-scholar-rss-feed` on every push to `main`, on `v*` tags, and via
manual dispatch. It uses the built-in `GITHUB_TOKEN` — no extra secrets required.

1. Push this repo to GitHub (the workflow runs automatically).
2. To let the NAS pull without logging in, make the package public once:
   **GitHub → your profile → Packages → `google-scholar-rss-feed` → Package settings →
   Change visibility → Public.**
   (Alternatively, keep it private and run `docker login ghcr.io` on the NAS with a personal
   access token that has `read:packages`.)

### Deploying on a Synology NAS (Container Manager)

Tested on a DS423+ (x86_64). Any Intel/AMD Synology with Container Manager works the same way.
Because the image is prebuilt, the NAS only pulls and runs it.

1. Put a small deployment folder on the NAS, e.g. `/volume1/docker/scholar-rss`, containing
   just your edited `feeds.toml` and a `docker-compose.yml`. You can `git clone` the whole
   repo there, or create the folder with only these two files:

   ```yaml
   # docker-compose.yml (NAS)
   services:
     scholar-rss:
       image: ghcr.io/michaelmvh/google-scholar-rss-feed:latest
       container_name: scholar-rss
       ports:
         - "3005:3005"
       volumes:
         - ./feeds.toml:/config/feeds.toml:ro
       restart: unless-stopped
   ```

2. Open **Container Manager → Project → Create**, point it at that folder, and choose
   **Use existing docker-compose.yml**. Container Manager pulls the image and starts the
   container (no build step).
3. Reach the feed at `http://<nas-ip>:3005/?feed=myfield`.

**Updating:** in the Project, use **Pull** (or `docker compose pull && docker compose up -d`)
to grab the latest published image. To change which authors a feed tracks, just edit
`feeds.toml` — no pull, rebuild, or restart required.
