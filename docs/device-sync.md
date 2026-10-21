# Sony Walkman USB song management

Tyranny's macOS client talks directly to a local `tyranny-mtp` helper. The music
server and its wire protocol are not involved. The supported USB profile is the
Sony NW-A306 in File Transfer mode (`054c:0e6e`). Interface discovery retains
OpenMTP's bulk-in/bulk-out/interrupt-in endpoint detection and MTP interface
string validation, including Android vendor-specific interface classes.
Other USB devices are filtered **before opening or claiming their interfaces**.
Multiple supported devices are selected by stable USB identity, not enumeration
order; duplicate identities are rejected rather than guessed.

Unlock the Walkman, select **USB File Transfer**, and close OpenMTP, Android File
Transfer, or another application holding the MTP interface. Connect with a data
cable. Discovery only opens a session and reads device/storage information; it
does not recursively read music. Inventory and content matching are read-only.
A process-wide advisory lock serializes Tyranny USB sessions; libusb interface
claiming also reports contention with other applications. Discovery uses a
five-second per-transfer timeout; inventory and content operations use sixty
seconds for large SD cards. These are per-transfer, not whole-job deadlines.
A failing close does not reset the device; reconnect after a lost protocol session.

## Using Tyranny

The connected player appears under **Devices** in the sidebar. Click it to see
its audio files, switch between internal storage and SD card, search by filename
or path, and inspect the music/other/free storage bar at the bottom. Select songs
and choose **Delete Selected…**; confirmation removes device copies only.

Drag library rows onto the device's sidebar entry or its management screen.
Transfers preserve original bytes and tags; they are additive, not a mirror that
deletes unselected music. Storage selection applies to new copies; already-present
content is not duplicated onto another storage.

Connected devices also supply optional named columns in normal track tables.
Columns distinguish **Checking…**, **Synced**, **Not synced**, and a failed check.
Use the Columns menu to show or hide them. They disappear when the device is
unplugged. Only local-server library paths can be copied; remote-server paths
are not interpreted as files on this Mac. Device browsing and deletion continue
to work without the playback server.

Discovery runs every three seconds while USB is idle. Reading a large card is
asynchronous; playback and the app remain usable while the inventory is loaded.
Reopening the device reuses its current inventory instead of rescanning the card.
Do not disconnect during a copy. Refresh the device after making changes through
another application.


## Actual guarantees

- Identity is `usb:<vendor hex>:<product hex>:<UTF-8 serial as hex>`. A missing
  serial is an error. Handles are interpreted only within the selected device
  and storage. The helper never falls back to the first connected device.
- Inventory walks each storage directory-by-directory rather than issuing the
  Android all-objects query, which timed out on the attached SD card. It reconstructs
  full paths. Songs outside `Music/Tyranny`, including OpenMTP copies, are included.
  Audio classification uses filename extensions; folders and macOS `._` resource-fork
  sidecars are never songs.
  Capacity and free space come from `GetStorageInfo`, not local estimates.
- `match` hashes each local regular audio file and lazily streams same-size device
  candidates through SHA-256. It searches **all storages**, never infers a match
  from a title, basename, or size. It reads device content without saving a local
  copy. Digests are reused only within that operation after object revalidation;
  the helper intentionally does not persist unverified metadata-only hash caches.
  The client invalidates confirmed matches when library revision or device
  inventory changes; an explicit Refresh checks changes made outside Tyranny.
- `sync` uses the same content identity across all storages. A song already in
  internal storage is skipped even when SD storage is selected. A different song
  with the same filename is not skipped or overwritten. New audio goes under
  `Music/Tyranny`; the final filename contains a bounded source stem, its full
  SHA-256 digest, and its original extension. No transcoding is performed, and
  recognition as audio does not promise Walkman codec support.
- Before copying each missing file, the helper rechecks device/storage identity,
  writable access, and current free bytes. Completed earlier files remain
  completed if a later file does not fit; partial results accompany the error.
  Classic MTP uploads are limited to `2^32 - 13` bytes per file (less than 4 GiB).
- A random staging filename ends in `.tyranny-partial` and is excluded from song
  inventory. A durable local intent journal is written **before** creating that
  object. The helper reads back the complete uploaded object and verifies SHA-256
  before renaming it to the final filename or reporting a match. Source changes,
  short transfers, checksum mismatches, and failed commit renames cannot count as
  successful syncs.
- On the next sync to that storage, the journal identifies only its exact staging
  object and managed parent. Verified complete bytes are committed without a
  second upload; incomplete bytes are deleted and recopied. There is no generic
  deletion sweep. Unjournaled temporary files are never removed. If the journal
  or managed directory is externally changed, cleanup refuses the mutation.
  Directory timestamps may change as children are added, renamed, or removed;
  they are not directory identity. File metadata remains fully revalidated.
  Journals live in `~/Library/Application Support/tyranny/mtp`. Keep this directory
  for interrupted-copy recovery. Final content identity survives process restart
  without a database: existing audio is rehashed when needed.
- `delete` accepts explicit audio IDs **and the selected song snapshots**. It
  checks the complete selection against current path, name, size, and storage
  before deleting anything; when a snapshot includes SHA-256, content must also
  match. It re-reads object metadata and USB identity immediately before each
  deletion. It never deletes directories or broad storage contents. As with any
  MTP client, a device-side change that reuses a handle with identical metadata
  cannot be distinguished without a supplied content digest. A mid-batch device
  failure can leave a partially deleted selection; the response reports the error
  and refreshes inventory when the device remains reachable.
- Sync/delete attempt fresh inventory and free-space reads even after a partial
  failure. If refresh fails, the response omits `device` rather than returning
  stale state as current. Errors are not turned into successful empty inventories.
- MTP does not offer an atomic filesystem transaction or a device-wide lock
  against Android applications. Tyranny's checks protect its explicit operations,
  but concurrent changes made on the Walkman itself can still cause errors.

## Helper JSON interface

One request on stdin, then EOF; one response on stdout. Diagnostics go to stderr.
The helper duplicates the JSON output descriptor and redirects **file descriptor
1** to stderr before USB initialization, so native dependency output cannot
corrupt JSON. Errors remain JSON-decodable, including recovered Go panics.
Consumers must check the `error` property, not just process exit status.

Operations:

- `{"operation":"discover"}` → optional `devices` (omitted means none).
- `{"operation":"inventory","device_id":"..."}` → `device` with storages/songs.
- `{"operation":"match","device_id":"...","files":[{"path":"/absolute/song.flac"}]}`
  → verified `matches` across all storages; absent matches mean not found.
- `sync` adds a required `storage_id` to the same files request.
- `delete` requires `device_id`, `storage_id`, `object_ids`, and `expected_songs`
  containing the selected complete song records from inventory.

Every response includes integer `copied`/`skipped` and `matches`; each match has
`path`, `object_id`, `storage_id`, and `sha256`. Device records have `id`, `name`,
`storages`, and `songs`. Storage records contain `id`, `name`, `capacity`, `free`.
Song records contain `id`, `storage_id`, `path`, `name`, `size`, and optional
`sha256`. An `error` can coexist with partial matches. No hardware simulator or
fallback success path is shipped.

## OpenMTP findings and implementation choice

The inspected OpenMTP native module pins
[`go-mtpx` at `18f12db021cc`](https://github.com/ganeshrvel/go-mtpx/tree/18f12db021cc4256396748dbd2996d15853d7033)
and [`go-mtpfs` at `1c3302b3c476`](https://github.com/ganeshrvel/go-mtpfs/tree/1c3302b3c476f69e787a0339122a804dd518a007).
The local source of that dependency declaration is
`/Users/kit/source/openmtp/ffi/kalam/native/go.mod`; its upstream home is
[OpenMTP's native module](https://github.com/ganeshrvel/openmtp/blob/master/ffi/kalam/native/go.mod).

`go-mtpx` supplies high-level directory walking, upload/download, rename, and
remove operations. Its
[`Initialize`](https://github.com/ganeshrvel/go-mtpx/blob/18f12db021cc4256396748dbd2996d15853d7033/main.go#L18-L36)
selects a device with an empty selector and calls `Configure`;
[`go-mtpfs` selection](https://github.com/ganeshrvel/go-mtpfs/blob/1c3302b3c476f69e787a0339122a804dd518a007/mtp/select.go)
opens candidates and rejects multiple matches. The
[`Configure` implementation](https://github.com/ganeshrvel/go-mtpfs/blob/1c3302b3c476f69e787a0339122a804dd518a007/mtp/mtp.go#L659-L693)
can reset USB on session-open failure. Those policies do not fit Tyranny's stable
selection and nonintrusive discovery requirements.

Tyranny therefore reuses the **same mature native transport**, not a new MTP
protocol implementation: only the `mtp` package of that exact `go-mtpfs` revision
is vendored under `tools/mtp/internal/mtp`. It directly uses the upstream
[object/storage/session operations](https://github.com/ganeshrvel/go-mtpfs/blob/1c3302b3c476f69e787a0339122a804dd518a007/mtp/ops.go),
including `GetObjectHandles`, `GetObjectInfo`, `GetObject`, `SendObjectInfo`,
`SendObject`, `SetObjectPropValue`, and `DeleteObject`. The local modifications
restrict candidate selection to the supported Sony USB profile, verify the MTP
interface string before claiming USB, propagate claim errors, and remove
reset-on-close-failure. Tyranny calls `OpenSession`
rather than `Configure`. FUSE, Electron, and the high-level `go-mtpx` module are
not linked into the helper.

The pinned [`ganeshrvel/usb` binding](https://github.com/ganeshrvel/usb/tree/14d96f5ae403)
uses cgo and `pkg-config: libusb-1.0`. Its transport and `go-mtpfs` licenses are
BSD-3-Clause. [libusb 1.0.30](https://github.com/libusb/libusb/releases/tag/v1.0.30)
is LGPL-2.1-or-later. The source archive checksum is pinned in the build script
and matches the [Homebrew source formula](https://github.com/Homebrew/homebrew-core/blob/master/Formula/lib/libusb.rb)
inspected during implementation; runtime Homebrew is **not** used.

## Building and redistribution

Build prerequisites: macOS 14+, Xcode command-line tools, Go 1.23+, `pkg-config`,
`make`, `curl`, and standard archive tools. First build requires network access
for checksum-verified libusb source and the Go checksum-pinned USB module.

```sh
scripts/build_mtp.sh
```

This prints the pinned native prefix and writes `target/mtp/tyranny-mtp`.
`just build-client`, `just build-client-release`, and `scripts/build_macos.sh`
build the helper. The app packager copies it beside the Swift executable, bundles
its complete non-system dylib dependency closure into `Contents/Frameworks`,
rewrites load commands to `@loader_path`, includes it in
`Resources/native-dependencies.json`, and signs the dylib/helper before the app.
Only the pinned libusb prefix is approved; unexpected Homebrew dependencies fail
packaging rather than silently becoming runtime requirements.

`Contents/Resources/MTP` carries the BSD/Go licenses, libusb's LGPL license,
complete unmodified libusb source archive, build script, and replacement/re-sign
instructions in `NOTICE`. This retains LGPL dynamic-library replaceability.
Builds are native arm64 or x86_64, not universal binaries.

The libusb build forces the `pipe`/`fcntl` implementation: newer SDKs expose
`pipe2` even though it is unavailable at the app's macOS 14 minimum. Explicit
pinned include/library paths also enter cgo's build-cache key; changing only
`PKG_CONFIG_PATH` can otherwise reuse a dependency linked to an obsolete prefix.
Repeated packaging follows the live executable dependency graph, replaces only
validated native build outputs, and removes obsolete bundled dylibs.

## Verification commands (integration owner)

Deterministic tests use an in-memory **object transport** to exercise the real
inventory, hashing, upload, journal, recovery, and deletion engine. They do not
claim hardware compatibility. Build the pinned prefix first, then:

```sh
prefix="$(scripts/build_mtp.sh)"
(cd tools/mtp && PKG_CONFIG_PATH="$prefix/lib/pkgconfig" go test ./...)
printf '%s\n' '{"operation":"discover"}' | target/mtp/tyranny-mtp
```

Use the returned device ID in read-only inventory/match requests:

```sh
printf '%s\n' '{"operation":"inventory","device_id":"usb:054c:0e6e:31303039303735"}' | target/mtp/tyranny-mtp
```

The serial-specific ID above corresponds to the connected device observed during
integration; do not hardcode it in application code. For a different device use
its discovery result. To smoke-test matching, supply an absolute local audio path
in a `match` request. **Do not execute sync/delete against connected hardware
without the user's explicit permission.** Upload, retry-after-interruption, and
removal require a separately authorized disposable fixture. A build or transport
unit test is not evidence that hardware mutation acceptance has been exercised.

## Observed hardware validation

On 2026-10-21, the connected NW-A306 exposed internal storage and a SanDisk SD
card. Tyranny listed 34 and 7,745 songs respectively; 139 macOS resource-fork
sidecars on the card were correctly excluded without removing them. The first
complete directory walk took approximately 39 seconds.

The native Accessibility-driven smoke exercised the real sidebar, storage picker,
both capacity bars, and drag/drop using an explicitly authorized disposable WAV:

- First drop created exactly one device song.
- Repeating the drop reported zero copies and one existing content match.
- The library track's membership became Synced.
- Canceling deletion preserved the device copy; confirming removed only that copy.
- The original device song-ID set was restored, and the local original remained.
- Device management remained available after disconnecting the playback server.

The final successful report and screenshots are under `target/device-native-final/`,
including the visibly rendered Sony WALKMAN **Synced** column.
The full Swift suite passed 440 tests; focused selection/capacity coverage passed
six tests. Nine native transport tests and nine real Mach-O packaging tests passed.
The packaged app passed strict deep code-signature verification.
A complete staged fixture from a rejected commit was also recovered on hardware:
SHA-256 matched, no second upload occurred, and deleting that exact recovered
fixture restored the original 7,779-song inventory. The regression transport now
models Android's directory timestamp changes during child mutations.

Interrupted-upload recovery is covered by deterministic transport failures, not
by physically unplugging this user's player during a write. No existing user song
was uploaded, overwritten, or deleted during the hardware smoke.
