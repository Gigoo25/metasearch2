# TODO

Open work for this fork. Check items off when you approve them. Architecture
notes and the zimit design live in `HANDOFF.md`.

## Open

### Kagi-inspired features

- [ ] Custom domain ranking (raise / lower) — implemented, pending confirmation
  - A "ranking" menu under each result has plain HTML raise/lower/hide forms
    (no JavaScript); posting redirects back to the same search page.
  - Rules live in sqlite (`site_rules`) as subdomain-aware score multipliers.
    The settings page lists them with a per-rule *undo* form, an add form and
    a clear-all, so hiding a domain is reversible.
  - Kiwix results are keyed by ZIM book (`kiwix:<book>`) and can be sorted the
    same way; they are never hidden, only strongly demoted (weight 0 → 0.05).
  - Verified locally and in the container: hide removes the domain, undo
    restores it, raising `linuxmint.com` moved it from #3 to #1.
  - Optional domain leaderboard of aggregate votes is still open.

- [ ] Safe search
  - Levels off / moderate / strict, default off; HTML checkbox in Settings,
    stored in the database (`settings` table) like the theme.
  - Engine side: pass each engine's safe parameter (Bing `adlt=strict`, Brave
    `safesearch=strict`, ...).
  - Local side: domain blocklist plus keyword filtering of titles and
    descriptions; also image results and the image proxy.

- [ ] Small web
  - Sources are in place: Marginalia, Mwmbl, Wiby (weights 0.15/0.15/0.10).
  - Remaining: preset lists of big domains to demote/hide and small-web sites
    to boost, exposed as a Settings toggle (`[urls.weight]` defaults).
  - Consider a "small web only" mode that hides the big-platform list.
  - Reference list for later: <https://github.com/kagisearch/smallweb> (small
    web sites/directory to seed the boost list).

- Candidates: bangs (`!w`), keyword site boosts (`+term`), pinned sites,
  up/down votes feeding personal ranking.

### Postsearch infoboxes

- [ ] Infoboxes show more often — implemented, pending confirmation
  - Triggers scan the top 30 results (was 8), and engines are tried in a
    stable `Engine::all()` order instead of hash order, so which infobox wins
    is deterministic.
  - MDN was broken: the page moved from `header > h1`/`.section-content` to
    `h1`/`section.content-section`, and locale-less urls
    (`developer.mozilla.org/Web/API/...`) are now accepted too.
  - Empty-url results (wiby) are skipped in the shared parser instead of being
    recorded and logging `url is empty`.
  - Verified in the container: `javascript fetch api guide` now shows the MDN
    infobox, `minecraft redstone comparator` the minecraft one; 0 empty-url
    warnings since.

### Pagination

- [ ] Google-style pagination (pages "go forever")
  - Current state: every engine fetches one page; the merged response is a
    single page and `CacheKey` is query + tab + config.
  - Offsets: Bing `first = page * size + 1` + `count`, Brave `offset`, kiwix
    `start`/`pageLength`. DuckDuckGo's html endpoint paginates through a POST
    form (needs investigation); Mwmbl/Wiby/Marginalia expose no obvious
    offset.
  - Ranking: use global positions (`page * size + index`) and accumulate
    per-query results so dedupe/host diversity see the whole set.
  - Cache: add a page dimension to `CacheKey`, or cache cumulative result sets
    up to the deepest fetched page.
  - UI: plain `?page=N` pager (Prev/Next and page numbers); there is no total
    across engines, so pages continue until all engines come back empty.

### Personal result index

- [ ] Build a growing index from search results — implemented (v1), pending
  confirmation
  - sqlite + FTS5 (external content with triggers); one transaction per search
    records url/title/description/engines, bumps `seen_count` and links the
    query. Retention: `[database] max_results` plus `max_age_days` (default
    180) for entries not seen recently.
  - A new `index` engine (weight 0.30) matches queries against it, so offline
    searches and previously seen pages still return something.
  - Verified: 20k-row FTS test search under 1s; the container returns 20 index
    results for an uncached query after a related search.
  - Still open: click/visit counts feeding ranking, export/import, per-client
    vs shared (currently shared).

### Storage

- [x] One sqlite database for everything — implemented, pending confirmation
  - The result cache moved from json files into a `response_cache` table
    (same TTL semantics, SQL eviction); `[cache] dir` is gone and
    `[database] path` is the only location.
  - `config-container.toml` sets `path = "/cache/metasearch.db"` so the
    database sits on the persistent `/cache` volume; the resolved path is
    logged at startup. (Before this fix it defaulted to the container's
    ephemeral `/root/.cache`, which lost settings and cache on every
    restart.)
  - Theme and custom CSS moved from the `settings` cookie into a `settings`
    table; the settings page is plain HTML forms, no JavaScript.
  - Schema changes are `PRAGMA user_version` migrations; old `<hash>.json`
    cache files are inert and can be deleted from the data directory.
  - Durability: WAL, `[database] synchronous` (default `full`, i.e. fsync per
    commit), 8 MiB journal size limit and Immediate transactions for writes.
  - Startup `PRAGMA quick_check` (toggle `[database] quick_check`) quarantines
    an unopenable or corrupt database as `metasearch.db.corrupt-<ts>` (plus
    `-wal`/`-shm`) and starts fresh instead of failing.
  - Pending migrations are protected by a pre-migration backup copy
    (`metasearch.db.pre-migration.bak`) taken with sqlite's backup API.
  - The database is meant to live on local disk with a single writer
    instance; running two instances on one file is unsupported.

### LLM answers

- [ ] Local LLM answers with references
  - Goal: answer the query in natural language with `[1]`-style citations
    linking to the search results, fully local, CPU-only, small memory
    footprint, model auto-download, and optional (`[llm] enabled = false`).
  - Decisions before implementing:
    - Runtime: embedded llama.cpp (e.g. `llama-cpp-2`) vs a bundled
      `llama-server` process vs an existing OpenAI-compatible endpoint
      (Ollama/llama.cpp). Embedded/bundled keeps it self-contained;
      endpoint-only is least code but does not auto-download.
    - Default model / RAM budget: SmolLM2-360M-Instruct Q4 (~250 MB),
      Qwen2.5-0.5B-Instruct Q4_K_M (~400 MB, likely default), or a ~1 GB 1.5B
      class model.
    - Behavior: every query or only question-like ones; stream tokens or show
      the answer once done; unload the model after idle or keep it resident.
  - Integration: a post-search step (like the postsearch infoboxes) that feeds
    the top results (title/url/snippet) to the model with a strict prompt,
    parses citations, only links to provided results, caches the answer with
    the query and renders it above the results.
  - Config: `[llm] enabled`, `model_url`/`model`, `context_size`,
    `max_tokens`, `threads`, `idle_unload_secs`.

### zimit save button

- [ ] "Save offline" button in search results
  - Decisions made: sidecar worker container from the official
    `ghcr.io/openzim/zimit` image, file-based queue/status in the shared
    `/cache` volume (no Docker socket), whole-site crawl bounded by
    page/time/size limits.
  - Full design, security requirements and verification plan are in the
    "Next task" section of `HANDOFF.md`.

### Follow-ups (smaller)

- Ranking extras: engine-agreement bonus, aggregator demotion, per-engine
  quality weighting; collect more bad-result examples.
- Tune Marginalia/Mwmbl/Wiby weights once the small-web presets land.
- First search after container start waits ~20-30s on the CIFS corpus (kiwix
  library build plus a cold fulltext search). Consider a "kiwix is warming up"
  notice, or document the local-SSD / `book` filter fix.
- `/kiwix` proxy has no `Range` support and a 60s total timeout.
- Responses where some engines failed are cached for the fresh TTL (10 min),
  so a transient kiwix failure can stick to a query.
- clippy fails on newer toolchains with a few pre-existing lints
  (`src/engines/search/bing.rs`, `src/urls.rs` `HostAndPath::replace`); fix so
  CI stays green.

## Done

- [x] Improve result ranking (`improve result ranking and kiwix results`)
  - Query relevance multiplier (exact-title boost, partial-match penalty),
    host diversity, duplicate-title dedupe (including per-ZIM for kiwix),
    kiwix defaults (weight 1.0, `page_length` 10, `max_per_book` 5, namespace
    pages dropped), `RANKING_VERSION` in the cache fingerprint.
  - Verified: `youtube` ranks the main article above the channel page and
    categories; top-10 diversity improved across the test queries.
- [x] Engine overhaul (`rework engines: small-web sources, drop dead ones,
  pace requests`)
  - Removed Yep, RightDao, Stract (dead) and Google search + images (JS-only;
    autocomplete kept).
  - Added DuckDuckGo, Mwmbl, Wiby; http schemes preserved so http-only small
    sites keep working.
  - Non-2xx responses are errors; per-engine pacing (`min_interval_ms`
    override), cool-downs with backoff on 429/503/challenge pages, soft
    retry for Marginalia; autocomplete debounce + 60s cache.
- [x] Fix the scrolling background and scrollbar colors while keeping the
  results panel (`fix scrolling background and scrollbar colors`).
- [x] Favicon with `/favicon.ico` redirect (`add favicon`).
- [x] Everforest Dark/Light themes (`add everforest themes`).
- [x] Disk cache for merged results with stale-on-network-failure and
  request-dependent-answer exclusion (`add search result cache and kiwix
  engine`).
- [x] Kiwix engine over kiwix-serve's RSS mode, `public_url`/`book` options,
  longer client timeout, `ensure_ready` warm-up (`add search result cache and
  kiwix engine`).
- [x] Single-container deployment: bundled kiwix-serve, library rebuild from
  `/zim`, `/kiwix` reverse proxy, compose mounts and cache volume (`bundle
  kiwix-serve in the container and proxy it at /kiwix`).
- [x] Startup empty-response fix: metasearch starts immediately and searches
  wait for kiwix internally.
- [x] Handoff notes for the next agent (`add handoff notes`).
