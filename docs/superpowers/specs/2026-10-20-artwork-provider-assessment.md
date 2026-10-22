# Artwork provider feasibility — extra-stream

Task: TSK-extra-stream-f7xrq. Assessed 2026-10-20 for tyranny likes smoke's Rust server / SwiftUI local-library player. These are engineering constraints from published primary sources, not legal clearance. API accessibility and possession of credentials do not grant artwork rights.

## Approved scope

The user explicitly approved deferring Last.fm on 2026-10-20 and implementing ordered Cover Art Archive / Deezer / Discogs fallback, per-provider enable/order settings, and a Discogs token. Last.fm remains excluded pending separate artwork permission. Deezer requires a non-caching display path; Discogs requires adjacent attribution and freshness enforcement. The implementation follows these distinct policies rather than placing every provider's art in a permanent cache.

Worktree: `../tyranny-extra-stream`, branch `feature/extra-stream-artwork`, created from `dev`. Implementation and exercised evidence are recorded in [SPIKE_RESULTS.md](../../../SPIKE_RESULTS.md#configurable-artwork-providers-2026-10-20).

## Original implementation constraints (before this task)

- `src/artwork/mod.rs`: two bounded workers, independent of playback; embedded then folder discovery; remote lookup currently requires a MusicBrainz release UUID and only calls Cover Art Archive. Positive remote results currently have no expiry.
- `src/artwork/provider.rs`: fixed origin, bounded response bodies, end-to-end deadline, serialized request starts, manually checked redirects. Preserve these protections when adding providers; API-returned URLs are untrusted.
- `src/artwork/cache.rs`: disposable SQLite image cache. Cache policy must become provider-specific; a successful fetch is not permission for permanent storage.
- `macos/Sources/TyrannyLikesSmoke/ArtworkStore.swift`: separate thumbnail/display connections and an in-memory image cache. Deezer's restriction is not documented as disk-only; bypass reusable image caching, not merely SQLite. Retain only the image needed for current display, pending scope approval.
- `ArtworkView.swift`: source currently appears in a tooltip. That does not satisfy Discogs' directly adjacent linked attribution requirement. Images also reach Now Playing/notification surfaces; provider provenance and permitted use need to survive every consumer, or restricted artwork must not reach those surfaces.
- Proposed invariants, not implemented: embedded/folder precedence remains fixed; user order affects only remote fallback; bounded exact album/album-artist matching rather than first search hit; provider-local failure must not block later providers; credentials never enter logs, snapshots or CDN requests; source URL and freshness accompany downloaded artwork.

## Deezer

**Access:** Unauthenticated album search is operational. Live GET of [search for Daft Punk Discovery](https://api.deezer.com/search/album?q=Daft%20Punk%20Discovery&limit=2) returned `Discovery` / `Daft Punk` (302127) and `Discovery` / **Data Punk** (74901252). Thus first-result matching is incorrect. Fielded `artist:"Daft Punk" album:"Discovery"` on `/search/album` returned zero results. Use text search and validate full returned album/artist identity; do not assume the general search DSL works here.

Results include `id`, `title`, `artist.name`, `link`, and sized `cover_*` URLs. The observed image host was `cdn-images.dzcdn.net`; the cover endpoint uses `api.deezer.com`. These are observed hosts, not a complete official allowlist. Use returned HTTPS URLs, validate every redirect, and do not synthesize CDN paths.

**Cache restriction:** The [official developer FAQ](https://support.deezer.com/hc/en-gb/articles/360011538897-Deezer-FAQs-For-Developers) asks “Can I store Deezer images as cache?” and answers “No, images are not allowed to be stored for legal reasons.” It does not grant a RAM-cache exception. Artist/track-name storage is separately permitted.

**Other conditions:** [Developer terms](https://developers.deezer.com/termsofuse) restrict standard use to noncommercial/private purposes and preserve artwork rights; no unrestricted reproduction permission is implied. [Logo guidelines](https://developers.deezer.com/guidelines/logo) require a clearly visible Deezer logo. The FAQ confirms a quota, but the linked numerical quota documentation redirected to login; no numerical allowance was verified. Serialize conservatively and respect refusals rather than assuming a published quota.

The bundled `macos/Resources/DeezerLogo.png` is the unmodified official brand site's heart logo: [official logo guidelines](https://developers.deezer.com/guidelines/logo) link to [deezerbrand.com](https://deezerbrand.com/), whose icon resource is [this PNG](https://media.ffycdn.net/eu/deezer/fWVrZctigy7YRqgUp1My.png?width=200&rect=0,0,200,200&reference_width=201). `scripts/build_macos.sh` copies it into the app's Resources. The client does not display Deezer art if the required branding asset is absent.

## Last.fm

**API shape:** [album.getInfo](https://www.last.fm/api/show/album.getInfo) accepts `api_key`, artist/album or MBID, optional `autocorrect`, and `format=json` at `https://ws.audioscrobbler.com/2.0/`. This read method needs no signed user session. [REST rules](https://www.last.fm/api/rest) describe image objects with `size` and `#text`. Validate returned identity, missing images and JSON error payloads; HTTP success alone is insufficient. No authenticated success was exercised.

**Blocker:** [API ToS §5.1.8](https://www.last.fm/api/tos) states:

> You will not use any audio, audiovisual, images and/or artwork, whether or not accessible through the API, and all such content is expressly excluded from this Agreement;

This is a separate sentence after the unauthorized-sharing prohibition. No personal/local-library exception is stated. An API key or opt-in checkbox does not override it. Ask `partners@last.fm` for separate permission/clarification. The general data license and 100 MB data cap do not remove the explicit artwork exclusion. If permission is obtained, review its specific attribution/cache terms rather than assuming the standard data rules suffice.

## Discogs

**API/auth:** [Official developer documentation](https://www.discogs.com/developers) documents personal-token authentication via `Authorization: Discogs token=...`, authenticated database search, release details, a unique User-Agent, and 60 authenticated requests/minute (25 unauthenticated). Read rate-limit headers; do not send the token to image hosts. Static docs returned 403 but were readable in Chromium. No authenticated success was exercised.

Search: `https://api.discogs.com/database/search?type=release&artist=...&release_title=...`; release details: `https://api.discogs.com/releases/{id}`. Validate separate release title/artists rather than splitting combined search titles on a hyphen. Prefer primary artwork and preserve edition ambiguity. Observed image host: `i.discogs.com`; image URLs are signed and must remain unchanged. Public requests sometimes expose data contrary to the documented authentication table; do not rely on that as a stable keyless contract.

**Terms:** [API Terms of Use](https://support.discogs.com/hc/en-us/articles/360009334593-API-Terms-of-Use) classify images as Restricted Data, not CC0 catalogue metadata. Personal noncommercial application use is conditionally permitted; commercial use and third-party transfer of Restricted Data are prohibited, and downstream image-rights compliance remains the user's responsibility.

Content must not be displayed more than six hours stale; retain only as necessary. Refresh/revalidate by six hours and suppress expired art when offline. Required directly adjacent, linked notice: **“Data provided by Discogs.”** linked to the corresponding Discogs page. Settings-only acknowledgement is insufficient. Required application notice, permitted in usage documentation:

> This application uses Discogs’ API but is not affiliated with, sponsored or endorsed by Discogs. ‘Discogs’ is a trademark of Zink Media, LLC.

## Assessment-only providers

| Provider | Authentication/access | Suitability and missing prerequisite |
| --- | --- | --- |
| Musixmatch | Registered developer/plan; secret `apikey`. | Current public `album.get` and `artist.albums.get` schemas do not document cover URLs. Confirm an artwork endpoint and applicable display/cache rights before implementation. Default terms are noncommercial, require attribution and necessary third-party permissions. |
| TIDAL | Registered app client ID/secret; OAuth client credentials for catalogue, authorization code + PKCE for user resources. | Noncommercial standard license, mixed-service restrictions, branding/link requirements, temporary-only artwork cache, production use-case review. Obtain clearance for this specific local-library fallback; do not assume all local-file use is categorically banned. |
| Qobuz | Qobuz-issued application ID/secret. | Available official agreement is dated 2011 and limits use to registered users/approved devices, with a disclaimer. Current enrollment, API schema/auth details and cache rights remain unconfirmed. Obtain current partner terms; do not extract web-player credentials. |
| Spotify | Registered app client ID/secret and OAuth; client credentials for catalogue. | Attribution/logo/link, temporary-only cache, no cropping/overlay, mixed-service/core-experience restrictions. Development access and extended-quota eligibility are restrictive. Obtain exact use-case approval and viable access; do not misapply streaming-only rules as a blanket metadata ban. |

Primary sources:

- Musixmatch: [getting started](https://docs.musixmatch.com/getting-started), [album schema](https://docs.musixmatch.com/api-reference/lyrics-catalog/album-get), [artist albums](https://docs.musixmatch.com/api-reference/lyrics-catalog/artist-albums-get), [API terms](https://about.musixmatch.com/apiterms).
- TIDAL: [app registration](https://developer.tidal.com/documentation/api-sdk/api-sdk-manage-apps), [authorization](https://developer.tidal.com/documentation/api-sdk/api-sdk-authorization), [developer terms](https://developer.tidal.com/documentation/guidelines/guidelines-developer-terms), [guidelines](https://developer.tidal.com/documentation/guidelines/guidelines-developer-guidelines).
- Qobuz: [official API terms PDF](https://static.qobuz.com/apps/api/QobuzAPI-TermsofUse.pdf). The old document is evidence of historical terms, not a claim about the contract Qobuz would issue today.
- Spotify: [album endpoint](https://developer.spotify.com/documentation/web-api/reference/get-an-album), [client credentials](https://developer.spotify.com/documentation/web-api/tutorials/client-credentials-flow), [quota modes](https://developer.spotify.com/documentation/web-api/concepts/quota-modes), [policy](https://developer.spotify.com/policy), [terms](https://developer.spotify.com/terms).

## Existing MusicBrainz / Cover Art Archive

[MusicBrainz licensing](https://musicbrainz.org/doc/About/Data_License) distinguishes CC0 core metadata from supplementary licensed data; cover art is not part of that metadata license. [CAA policy](https://musicbrainz.org/doc/Cover_Art_Archive) preserves artists'/labels' rights and describes use at the user's own risk. Do not label covers CC0 simply because MusicBrainz metadata is CC0.

[MusicBrainz API](https://musicbrainz.org/doc/MusicBrainz_API) requires a meaningful User-Agent and at most one request/second. [CAA API](https://musicbrainz.org/doc/Cover_Art_Archive/API) has separate service rules; do not misattribute MusicBrainz's rate limit to CAA. Preserve release provenance and documented redirects. Reviewed CAA pages provide no universal cache TTL or blanket artwork reuse license.

## Verification and task state

Primary-source review completed for all eight named services. Live unauthenticated Deezer JSON lookup proved keyless access and the false-positive matching boundary. No image-use permission was inferred from that success. No real Discogs credential was supplied: authenticated retrieval is covered by local HTTP fixtures, not a live-account success claim. Subsequent daemon/native verification is recorded in [SPIKE_RESULTS.md](../../../SPIKE_RESULTS.md#configurable-artwork-providers-2026-10-20).
