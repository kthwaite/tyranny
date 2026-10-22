# Lyrics provider assessment

Assessed 2026-10-20 for TSK-smart-gale-rqmwc. This is an engineering assessment, not a determination of copyright clearance. Public access, attribution, an API key, and an open-source server license do not by themselves grant rights to reproduce lyrics.

## Decision

Implement embedded USLT/LYRICS first, then a locally cached, on-demand LRCLIB lookup. Assess the other requested providers only; do not add scrapers, credential placeholders, or fallback requests to them. LRCLIB explicitly documents public application access, making it the practical integration choice, but the sources reviewed do **not** establish blanket lyric-content redistribution rights. Resolve that separately before making licensing claims or distributing a bundled lyric corpus.

Provider facts below come from linked first-party documentation/source. “Not found” means not found in this assessment, not proof that no private partner API exists. Access failures are recorded rather than treated as permission.

## Provider matrix

| Provider | Documented access and authentication | Usage, attribution, and rights evidence | Decision |
| --- | --- | --- | --- |
| **LRCLIB** | [Official API](https://lrclib.net/docs): public JSON API, anonymous access allowed; identifying client header required. Optional application registration increases rate limits. Plain and synchronized lyrics supported. | API documentation specifies request identification, throttling, and rate-limit handling, but states no display-attribution requirement or blanket lyric-content license. [Repository MIT license](https://github.com/tranxuanthang/lrclib/blob/main/LICENSE) covers software and associated documentation. Copyright violations can be reported through the documented `/api/flag`. | **Enable now**, after embedded tags, with bounded local caching and visible provider credit/link. Do not equate API access or MIT software licensing with copyright clearance. |
| **Genius** | [Official API](https://docs.genius.com/) requires OAuth2 access tokens; an application client token supports non-user-specific read-only endpoints. Documented resources are search, song metadata, annotations, and referents; no standalone full-lyric-text retrieval endpoint is documented. | API docs require a license for commercial use. [Terms](https://genius.com/static/terms), “User Conduct” and “Intellectual Property Rights,” restrict automated collection/scraping and unauthorized copying; commercial use requires express written consent. Token access is not permission to scrape song pages. Attribution/display rights for a proposed full-lyrics integration remain unestablished. | **Defer** until a permitted full-lyrics delivery path and applicable license/display terms are agreed. No HTML extraction. |
| **Musixmatch** | [Getting started](https://docs.musixmatch.com/getting-started.md) documents registration and a secret `apikey`; [catalog API](https://docs.musixmatch.com/api-reference/lyrics-catalog/lyrics-catalog-overview.md) offers lyric and synchronized-subtitle endpoints. | [API terms](https://about.musixmatch.com/apiterms), §§1–3, require applicable fees/permissions, protect keys, require credit, and prohibit non-API harvesting; commercial use needs express approval. [Display guidelines](https://docs.musixmatch.com/implementation-guidelines.md) require view tracking, `lyrics_copyright`, country restrictions, a “Lyrics powered by” image, and `backlink_url`. Terms §7 require deleting API data on termination. | **Defer** pending account/plan, licensing, key provisioning, permitted caching, territory handling, and required display/tracking implementation. Do not embed a shared secret in the native app. |
| **lyrics.ovh** | [Official API specification](https://github.com/NTag/lyrics.ovh/blob/main/apiary.apib): `GET https://api.lyrics.ovh/v1/{artist}/{title}`, JSON `lyrics` on 200 or `error` on 404; no authentication is specified. No synchronized output or source attribution field is specified. | [Maintainer README](https://github.com/NTag/lyrics.ovh) describes LRCLIB first, then third-party sources; [retrieval code](https://github.com/NTag/lyrics.ovh/blob/main/lyrics.js) includes HTML fetching/parsing. The README acknowledges blocked sources and disables several on its own server. No downstream lyric-content rights grant or attribution contract was found in the API specification/README. Its MIT source license is not evidence of rights to upstream lyrics. | **Defer**: duplicates LRCLIB while introducing opaque upstream provenance and scraping/rights uncertainty. A public proxy does not remove upstream restrictions. |
| **SongLyrics** | No official public lyric API/authentication contract found; the [site terms](https://www.songlyrics.com/termsconditions) describe an advertising-supported website without user accounts. | Terms §2 expressly prohibit automated copying, scraping, or harvesting and substantial republication; §3 reserves lyrics to their rights holders and provides them for personal reference. No third-party display/attribution license is established. | **Defer; no scraping.** Require express provider/rights-holder permission and a supported delivery contract. |
| **AZLyrics** | No official public lyric API/authentication contract found. Both the [homepage](https://www.azlyrics.com/) and [copyright page](https://www.azlyrics.com/copyright.html) returned the site's browser/robot access challenge during direct retrieval. | Search indexed the copyright page as a DMCA policy, but its full content and any broader current terms could not be verified. Authentication, reuse, caching, and attribution permissions remain **unknown**. The challenge is not a license or an invitation to bypass access controls. | **Defer** until official access and rights terms can be verified. No challenge bypass or scraping. |
| **eLyrics** | No official public lyric API/authentication contract found. [Terms of service](https://www.elyrics.net/tos.html) describe personal, non-commercial search/display. | Terms prohibit assigning, copying, transferring, or transmitting lyrics to third parties and evading protections; lyrics remain their owners' property. The [site footer](https://www.elyrics.net/) additionally says copying, redistributing, and printing are not allowed (present in page HTML). No third-party application display/attribution license established. | **Defer** pending explicit permission and supported access. Do not use personal site access as a redistribution grant. |
| **Letras** | No official public lyric API/authentication contract found in the reviewed sources. The official [Spanish legal page](https://www.letras.com/aviso-legal.html) and [Portuguese legal page](https://www.letras.mus.br/aviso-legal.html) both returned HTTP 403. | Search confirms the legal page exists, but current substantive reuse/scraping, caching, and attribution terms could not be verified. Those permissions remain **unknown**, not unrestricted. | **Defer** until official terms and a permitted API/partner integration are available. No scraping. |

## LRCLIB integration contract

Source: [official API documentation](https://lrclib.net/docs), especially “Get lyrics with a track's signature,” “Lyrics record fields,” and “Implementation requirements.”

### Request and matching

Use `GET https://lrclib.net/api/get` with URL-encoded parameters:

| Parameter | Documented requirement |
| --- | --- |
| `track_name` | Required string; track title. |
| `artist_name` | Required string; artist name. |
| `album_name` | Optional string, recommended for precision. |
| `duration` | Optional number of seconds, recommended; when supplied, must be between 1 and 3600. Matching tolerance is ±2 seconds. |

Use authoritative local track metadata and duration. Do not broaden a failed exact lookup into fuzzy search or silently omit a known out-of-range duration to find a different recording. `/api/search` is a separate, less constrained search interface, not this integration's fallback. A missing record can become available later through LRCLIB's background fetching, so negative caching must expire.

### Successful record fields

| JSON field | Documented type / meaning |
| --- | --- |
| `id` | Number; stable lyric-record identifier. |
| `name` | String; backwards-compatible alias of `trackName`. |
| `trackName`, `artistName`, `albumName` | Strings; matched recording metadata. |
| `duration` | Number; seconds. |
| `instrumental` | Boolean; explicitly indicates no lyrics. |
| `plainLyrics` | String or null; unsynchronized text. Null for instrumental records. |
| `syncedLyrics` | String or null; line-timestamped LRC. Null for instrumental records. |
| `hasWordSync` | Boolean; indicates word timings in `lyricsfile`. |
| `lyricsfile` | String; raw Lyricsfile YAML, documented as present on every record. |

tyranny likes smoke consumes plain text and line-synchronized LRC only; it need not parse Lyricsfile YAML or word timings. Accept additional provider fields without changing the wire protocol.

### Outcome mapping and service etiquette

These are tyranny likes smoke integration decisions unless explicitly attributed to the API documentation:

- Nonempty embedded lyrics take precedence and require no network request. Streams return `missing`; do not search arbitrary stream metadata.
- HTTP 200 with `instrumental: true` maps to `instrumental`, not a network failure. Otherwise usable plain/LRC text maps to `ready`. An empty non-instrumental record does not establish that a track is instrumental.
- HTTP 404 (`TrackNotFound`) maps to an expiring `missing` result. Timeouts, malformed responses, 400/401, 429, and 5xx are `unavailable`, not permanent negative matches.
- Identify requests with `User-Agent: tyranny-likes-smoke/<version> (https://github.com/kthwaite/tyranny)`. LRCLIB requires app name/version and a project URL or contact; it allows `X-User-Agent` or `Lrclib-Client` when `User-Agent` cannot be set.
- The docs require honoring `Retry-After` for 429 and document it for 503; wait at least that long before another attempt, with jitter rather than a synchronized retry burst. Do not rely on an error body being JSON. Sequential requests and a 200–500 ms delay are recommended, especially for batch work. Do not scan the whole library for lyrics.
- Registration is optional. Public `Lrclib-App-Id` identifies registered client-side apps; secret `Lrclib-Api-Key` identifies server-side registrations. Anonymous access is adequate for this slice; do not ship invented credentials or promise fixed quotas, which the docs explicitly say may change.
- Bound the local cache; expire missing and transient results, and invalidate embedded discovery when files change. Successful online results remain available offline until eviction. Keep transient service failures distinguishable from misses. Run discovery/network work outside the playback owner and use a separate native request connection.
- Display “Lyrics: LRCLIB” with a provider link; a record URL uses `https://lrclib.net/tracks/{id}` (an [official indexed record page](https://lrclib.net/tracks/13352860) establishes that route). This credit is **tyranny likes smoke policy**, not a claim that current API docs mandate that wording or confer copyright permission. Embedded lyrics should be labeled as embedded, not attributed to LRCLIB.

No provider credentials, production database changes, live-app changes, or builds/tests were needed for this assessment.
