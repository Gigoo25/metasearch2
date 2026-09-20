# Handoff notes

State of this fork after the offline / single-container work. Read this before
starting the next task (the zimit "save offline" button).

Commits at the time of writing:

- `add search result cache and kiwix engine` (b626802)
- `bundle kiwix-serve in the container and proxy it at /kiwix` (ed1ee6a)
- `add everforest themes` (94b29db)

## What is implemented

### Search result cache (`src/engines/cache.rs`)

- Caches the merged response (results, featured snippet, answer, infobox) plus
  the post-search infobox, keyed by normalized query + tab + a deterministic
  fingerprint of the engine/url config.
- Fresh entries (default 600s) are served without contacting engines. Expired
  entries (default 604800s) are served only when every network engine failed.
- `is_cacheable` refuses answers from request/time-dependent engines (`ip`,
  `useragent`, `timezone`, `fend`), and responses from runs where all network
  engines failed are never stored (prevents offline runs from poisoning it).
- JSON files in `cache.dir` (`$XDG_CACHE_HOME/metasearch` by default, `/cache`
  in the container), atomic writes, LRU-by-mtime eviction (`max_entries`).

### Kiwix engine (`src/engines/search/kiwix.rs`)

- Queries kiwix-serve's RSS mode (`/search?...&format=xml`) and maps items to
  results. Config lives in `[engines.kiwix]`:
  - `url`: server-side address of kiwix (e.g. `http://127.0.0.1:8090/kiwix`).
  - `public_url`: link base shown to the browser. Empty string keeps kiwix's
    own relative links (`/kiwix/content/...`) — used by the container.
  - `book`: restrict to one book by its kiwix *name* (e.g. `wikipedia_en_all`,
    not the `.zim` file name).
  - `page_length`.
- Uses `KIWIX_CLIENT` (60s timeout) because cold multi-book searches exceed the
  shared 10s `CLIENT` timeout.
- `ensure_ready` waits for kiwix to come up and warms its searchers; searches
  wait on it internally so the web UI never blocks on kiwix startup.
- Relative result URLs pass through `src/urls.rs` untouched (no error logs).

### Single-container deployment

- `Containerfile` runtime installs `kiwix-tools` + `curl`, copies
  `config-container.toml`, `kiwix-empty-library.xml` and `docker-entrypoint.sh`.
- `docker-entrypoint.sh` runs one background subshell that rebuilds the kiwix
  library from `$KIWIX_ZIM_DIR` (default `/zim`, recursive) into
  `/cache/kiwix-library.xml` and then runs
  `kiwix-serve --address=127.0.0.1 --urlRootLocation=/kiwix -M`.
  Metasearch starts immediately (PID 1); kiwix comes up behind it.
- `src/web/kiwix_proxy.rs` serves `/kiwix` and `/kiwix/{*path}` by proxying to
  the configured kiwix URL (streaming body; forwards content-type/etag/
  last-modified/cache-control/content-disposition). No `Range` support.
- `config-container.toml` enables kiwix with `url = "http://127.0.0.1:8090/kiwix"`
  and `public_url = ""`, and puts the cache in `/cache`.
- `compose.yml` mounts `./zim:/zim:ro` and the `metasearch-cache` volume.

### Themes

- `src/web/assets/themes/everforest.css` (Dark Medium) and
  `everforest-light.css` (Light Medium), registered as static routes and listed
  in the settings dropdown (`src/web/settings.rs`).

## Verified behavior

- `cargo test`: 15 tests pass.
- Image builds with `docker build -f Containerfile -t metasearch-single .`.
- With a real 15-ZIM corpus (bind-mounted from a CIFS share): index page
  responds in about a second, static CSS 200, Kiwix results carry
  `/kiwix/content/...` links and article content is served through the single
  port (200 `text/html`).
- Restart: cached query served in ~12 ms; library rebuilt from `/zim` at start.
- The first search after container start waits for the library build (~20s on
  the CIFS corpus) plus a cold kiwix FTS (~9s). Later *new* queries take ~8-9s;
  repeats are fast (kiwix search cache ~0.5s, metasearch cache instant).
- The search slowness is I/O, not CPU: the 127 GB Wikipedia ZIM lives on CIFS.
  Faster options: local SSD, or `[engines.kiwix] book = "wikipedia_en_all"`.
  Streaming web results before kiwix would need a ranking refactor.

## Known limitations

- Kiwix cold fulltext search is 7-9s per new query on the CIFS corpus.
- The `/kiwix` proxy doesn't forward `Range` and has a 60s total timeout.
- A search where some engines fail is still cached for the fresh TTL, so a
  transient kiwix failure can stick to a query for ~10 minutes.
- ZIMs without a fulltext index are browsable but not searchable.
- ZIMs added live with `docker exec ... kiwix-manage add` are forgotten on
  restart; the `/zim` directory is the source of truth.
- `HANDOFF.md` is intentionally excluded from the image by `.dockerignore`.

## Next task: zimit "save offline" button

Decisions already made with the user:

- Runner: **sidecar worker container**, based on the official
  `ghcr.io/openzim/zimit` image plus a small worker script. Jobs and status are
  files in the shared `/cache` volume — no Docker socket, no HTTP service in the
  worker.
- Scope: **whole site**, bounded by page/time/size limits from config.

### Design

Config (`[zimit]`, default `enabled = false`):

```toml
[zimit]
enabled = false
dir = ""                # default: <cache.dir>/zimit
page_limit = 100
time_limit_secs = 3600
size_limit_bytes = 500000000
```

Backend:

- `POST /zimit` with JSON `{ "url": ... }`:
  - 403 unless `[zimit] enabled`.
  - Same-origin check: if an `Origin` header is present its host must match the
    `Host` header (the instance has no auth, this is the only CSRF guard).
  - Validate with `url_jail::validate(url, Policy::PublicOnly)` (blocks SSRF to
    private/local addresses).
  - Reject if the same URL is already queued or running.
  - Sanitize a job name from the host plus a short id; write
    `queue/<id>.json` = `{id, url, name, page_limit, time_limit_secs,
    size_limit_bytes, requested_at}` atomically; return `{id}`.
- `GET /zimit/status` reads `queue/`, `running/` and `results/` and returns the
  jobs (`queued` | `crawling` | `done` | `failed`, with `zim` path or `error`).
- Background task spawned in `web::run`: every few seconds scan `results/` for
  `done` jobs without `imported = true`, run
  `kiwix-manage /cache/kiwix-library.xml add <zim>` and mark the result file
  imported. `kiwix-serve -M` reloads the library automatically.
- Entrypoint: when rebuilding the library at startup, also scan the zimit
  output directory (e.g. `/cache/zimit/zim`) so crawled ZIMs survive restarts.

Worker (`zimit-worker.sh`, mounted into the sidecar; the image has `bash` and
`python3`):

- Loop: take the oldest `queue/*.json`, move it to `running/`, parse fields,
  then run
  `zimit --seeds URL --name NAME --output /cache/zimit/zim --pageLimit N --timeSoftLimit S --sizeSoftLimit B`,
  log to `logs/<id>.log`, and write `results/<id>.json` with `done` + the new
  `.zim` path or `failed` + the log tail. Then remove the `running/` file.
- One job at a time; ignore malformed queue files (log and remove).

UI:

- `src/web/search/all.rs::render_search_result`: render a
  `<button class="zimit-save" data-url="...">` for http(s) results, only when
  `config.zimit.enabled`.
- `src/web/assets/script.js`: on click, `POST /zimit`, then poll
  `GET /zimit/status` every few seconds; update the button with
  queued/crawling/saved/failed. Add a small rule to
  `src/web/assets/style.css`.
- Do not show the button for Kiwix results (already offline).

Compose:

- Add a `zimit` service using `image: ghcr.io/openzim/zimit`, override the
  entrypoint to `/worker.sh`, mount `metasearch-cache:/cache` and
  `./zimit-worker.sh:/worker.sh:ro`.

### zimit facts (verified upstream)

- Image `ghcr.io/openzim/zimit` is `FROM webrecorder/browsertrix-crawler`
  (includes Chromium and Node) plus a Python venv with zimit/warc2zim.
- CLI: `zimit --seeds URL --name NAME [--output DIR] [--pageLimit N]
  [--timeSoftLimit S] [--sizeSoftLimit B] ...`. It calls warc2zim internally;
  the output is a `.zim` named after `--name`.
- The image entrypoint appends ad blocklists to `/etc/hosts`; the worker should
  either do the same or use the image entrypoint before running zimit.
- `kiwix-manage LIBRARY add ZIMPATH` + `kiwix-serve -M` gives live library
  updates (verified in this session).
- zimit is not bundled into the metasearch image (headless browser would add
  ~1.5-2 GB and a lot of maintenance).

### Suggested verification

- Unit tests: URL/name validation, queue/status file parsing, duplicate
  rejection, `enabled = false` behavior.
- Local e2e: run the sidecar against a tiny static site (or `example.com`),
  assert a ZIM is produced, added to the library, and findable through the
  kiwix engine; check the button only renders when enabled and that a
  cross-origin POST is rejected.

## Build and test commands

```sh
cargo test
cargo fmt --check
docker build -f Containerfile -t metasearch-single .
docker run -d --name metasearch -p 28019:28019 \
  -v <zim-dir>:/zim:ro -v metasearch-cache:/cache metasearch-single
curl -s http://localhost:28019/ | head
curl -s 'http://localhost:28019/search?q=linux' | grep -c /kiwix/content/
curl -so /dev/null -w '%{http_code}\n' http://localhost:28019/themes/everforest.css
```

Notes for the next agent:

- The dev host builds Rust in the project's Nix dev shell. Outside it, the
  boring-sys build needs `LIBCLANG_PATH` and `BINDGEN_EXTRA_CLANG_ARGS` from
  `nix develop`, plus `gnumake` (e.g. `nix shell nixpkgs#gnumake`).
- `docker build` needs `-f Containerfile`; a plain `docker build .` looks for
  `Dockerfile` and fails.
- The Containerfile's pinned `perl`/`curl` versions were updated during this
  work because the older pins disappeared from Debian trixie. If apt fails
  again, refresh those pins the same way.
