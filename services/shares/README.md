# R2 sharing beta

This Worker accepts bounded screen-take uploads and serves unlisted public pages at `https://see.computer/s/<share-id>/`. It uses one SQLite Durable Object for the beta. That object owns share state, quota decisions, and upload byte accounting. R2 stays private.

## Limits and identity

An installation creates a random 32-byte secret and sends its 64 lowercase hexadecimal characters as `Authorization: Bearer <secret>`. The Worker hashes the secret with SHA-256 before it reaches the coordinator. It never stores or logs the raw value. There is no master upload token.

The secret identifies one installation, not one person. The service accepts any correctly formed random secret. The IP quota and global quota bound abuse during the beta. The coordinator stores only the SHA-256 hash of `CF-Connecting-IP`.

The enforced limits are:

- 10 new shares per installation per UTC month
- 20 new shares per IP hash per UTC day
- 100 new shares across the beta per UTC day
- 1,500 files and 256 MiB per share
- 8 MiB per file
- Expiry no later than 30 days after creation

Creation counters include shares that are later canceled. An empty canceled share can consume one creation. A retry of the same active share ID with the same owner, URL, and expiry does not consume another creation.

## Request sequence

Generate the installation secret locally. Keep it in app storage and never place it in a URL.

```sh
openssl rand -hex 32
```

The app generates a 7-character share ID from the base62 alphabet. Shares from the first beta build keep their 32-character hexadecimal IDs. It sends an empty POST to create the share. The server returns the URL and a 30-day expiry. Empty POST retries reuse the original expiry. API clients may also choose an earlier expiry with this optional body.

```http
POST /api/shares/<id>
Authorization: Bearer <64 lowercase hexadecimal characters>
Content-Type: application/json

{"url":"https://see.computer/s/<id>/","expiresAt":"2026-10-01T12:00:00.000Z"}
```

Upload each file while the share is open.

```http
PUT /api/shares/<id>/shots/001.jpg
Authorization: Bearer <installation secret>
Content-Length: 12345

<bytes>
```

The Worker also accepts chunked request bodies. It counts the bytes read and stops above 8 MiB. A supplied `Content-Length` must match the bytes received. Upload retries replace the same path and charge only the byte difference. Relative object paths use a restricted ASCII character set and can be at most 128 characters.

The coordinator reserves a replacement's byte delta and file-count delta before writing R2. It commits the reservation after R2 accepts the object. An R2 error rolls it back. If the request stops between those steps, the next mutation for that share compares the private R2 content hash with the reservation and either commits or rolls back. Public reads require the same hash as the committed SQLite row, so an unfinished replacement stays hidden.

Storage calls for one share run in the order they arrive. A slow R2 write holds back only its own share. Creating a share never waits on R2. If the share is canceled or expires while a write is in flight, the coordinator refuses that write and releases its reservation.

Allowed extensions are `png`, `jpg`, `mp4`, `mov`, `m4s`, `m3u8`, `json`, and `md`. The `mov` type supports existing packaged takes and is served as `video/quicktime`. New recordings should upload HLS under `clips/001/index.m3u8` or a fragmented `fullclip.mp4`. Safari plays HLS natively. Chrome and other Media Source browsers use the self-hosted hls.js 1.7.2 light player. The page fetches the player only for an HLS clip that the browser cannot play itself. That includes a browser that reports HLS support and then fails to parse the clip, as Chrome 158 does with a take that is still recording. Its npm archive integrity is verified when vendoring; its license is included in public/hls-LICENSE.

During recording, upload `live.json` with this shape:

```json
{
  "clips": [{ "playlist": "live/001/index.m3u8" }],
  "shots": [{ "file": "shots/001.jpg" }]
}
```

The public page polls `status.json` and `live.json`. A capture that stays listed keeps its element, so a clip keeps playing while screenshots arrive. Uploaded paths pass through the same extension and path allowlist before the page creates an element. Transcript and manifest text enter the document through `textContent`.

After `transcript.json` and `take.md` exist, mark the share ready with an empty body.

```http
POST /api/shares/<id>/complete
Authorization: Bearer <installation secret>
```

Cancel with an authenticated delete. The coordinator writes a tombstone at once, so public reads stop even while a write for that share is in flight. It lists and deletes every R2 object after that write settles. A second delete by the same owner is safe. A canceled ID cannot be recreated.

```http
DELETE /api/shares/<id>
Authorization: Bearer <installation secret>
```

Public files support `GET`, `HEAD`, one byte range, `ETag`, and `If-None-Match`. A response takes its validator, its length and its bytes from one R2 read, so a file replaced during the request is served as one version. `take.md?absolute=1` rewrites allowlisted relative Markdown file links to this share's absolute public URLs. It does not render uploaded Markdown as HTML.

## Test

Run the unit suite from this directory.

```sh
bun test worker.test.mjs
```

The tests run the production request handler and its SQL against an in-memory SQLite database and an in-memory R2 adapter. They run the page's own script against a small stand-in for the DOM. They do not contact Cloudflare.

## Create and deploy

The beta is deployed on the Cocco Ventures account. To create an equivalent deployment, use these commands.

```sh
cd services/shares
bunx wrangler r2 bucket create see-computer-shares --config wrangler.toml
bunx wrangler r2 bucket lifecycle add see-computer-shares delete-after-30-days --expire-days 30 --force --config wrangler.toml
bunx wrangler deploy --config wrangler.toml
```

The lifecycle rule is required. Application expiry blocks reads at the requested time. The R2 rule removes objects 30 days after each object upload, including objects left behind by an interrupted cleanup. SQLite tombstones remain so a canceled ID cannot return.

The service has no signup, per-person identity, billing, recovery for a lost installation secret, directory listing, or payment plans. The single coordinator intentionally limits throughput. It is enough for the 100-creation-per-day beta cap, not a design for an open public service.
