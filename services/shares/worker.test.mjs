import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import worker, {
  LIMITS,
  ShareCoordinatorService,
  SqliteMetadata,
  parseByteRange,
  rewriteMarkdownPaths,
  sha256Hex,
  validateRelativePath,
} from "./worker.mjs";
import { viewerHtml } from "./viewer.mjs";

const origin = "https://see.computer";
const initialNow = Date.UTC(2026, 8, 9, 12, 0, 0);
const ownerA = "a".repeat(64);
const ownerB = "b".repeat(64);
const ipA = "1".repeat(64);
const ipB = "2".repeat(64);

// The Durable Object storage API over SQLite, so the tests run the production SQL.
function storage(database) {
  return {
    sql: {
      exec(query, ...bindings) {
        if (/^\s*CREATE/.test(query)) {
          database.exec(query);
          return { toArray: () => [] };
        }
        const rows = database.query(query).all(...bindings);
        return { toArray: () => rows };
      },
    },
    transactionSync: (operation) => database.transaction(operation)(),
  };
}

class MemoryR2 {
  constructor() {
    this.objects = new Map();
    this.pageSize = 1000;
    this.failPut = false;
    this.holds = [];
  }

  // Stalls the next matching call until release(): before it touches the
  // object, or once it has with `after`. release(error) makes the call fail.
  hold(operation, key, { after = false } = {}) {
    const hold = { operation, key, after };
    hold.reached = new Promise((resolve) => { hold.arrive = resolve; });
    hold.released = new Promise((resolve) => { hold.release = resolve; });
    this.holds.push(hold);
    return hold;
  }

  async stall(operation, key, after = false) {
    const index = this.holds.findIndex((hold) => hold.operation === operation && hold.key === key && hold.after === after);
    if (index < 0) return;
    const [hold] = this.holds.splice(index, 1);
    hold.arrive();
    const error = await hold.released;
    if (error) throw error;
  }

  async put(key, value, options = {}) {
    await this.stall("put", key);
    if (this.failPut) throw new Error("R2 failure");
    const bytes = value instanceof Uint8Array ? value.slice() : new Uint8Array(value);
    const sha = options.customMetadata?.sha256 ?? await sha256Hex(bytes);
    this.objects.set(key, {
      bytes,
      size: bytes.byteLength,
      httpEtag: `"${sha}"`,
      customMetadata: { ...options.customMetadata },
      httpMetadata: { ...options.httpMetadata },
    });
    await this.stall("put", key, true);
  }

  describe(value) {
    return { size: value.size, httpEtag: value.httpEtag, customMetadata: value.customMetadata, httpMetadata: value.httpMetadata };
  }

  async head(key) {
    await this.stall("head", key);
    const value = this.objects.get(key);
    return value ? this.describe(value) : null;
  }

  async get(key, options = {}) {
    await this.stall("get", key);
    const value = this.objects.get(key);
    await this.stall("get", key, true);
    if (!value) return null;
    const offset = options.range?.offset ?? 0;
    const bytes = value.bytes.slice(offset, offset + (options.range?.length ?? value.size));
    return { ...this.describe(value), range: { offset, length: bytes.byteLength }, body: new Blob([bytes]).stream() };
  }

  async list(options) {
    const all = [...this.objects.keys()].filter((key) => key.startsWith(options.prefix)).sort();
    const offset = options.cursor ? Number(options.cursor) : 0;
    const keys = all.slice(offset, offset + this.pageSize);
    const next = offset + keys.length;
    return {
      objects: keys.map((key) => ({ key })),
      truncated: next < all.length,
      cursor: next < all.length ? String(next) : undefined,
    };
  }

  async delete(keys) {
    for (const key of Array.isArray(keys) ? keys : [keys]) this.objects.delete(key);
  }
}

// A new coordinator instance. Pass the database and bucket of an earlier one
// to restart it over the same stored state.
function fixture(database = new Database(":memory:"), bucket = new MemoryR2()) {
  let now = initialNow;
  const metadata = new SqliteMetadata(storage(database));
  const service = new ShareCoordinatorService(metadata, bucket, { now: () => now, publicOrigin: origin });
  return { database, metadata, bucket, service, setNow(value) { now = value; } };
}

function shareRows(metadata) {
  return metadata.sql.exec("SELECT * FROM shares").toArray();
}

// Whether the promise settles once requests that are free to run have run.
async function settled(promise) {
  let done = false;
  promise.then(() => { done = true; }, () => { done = true; });
  await new Promise((resolve) => setTimeout(resolve, 25));
  return done;
}

function id(number) {
  return number.toString(36).padStart(7, "0");
}

function internalRequest(path, { method = "GET", owner = ownerA, ip = ipA, body, headers = {} } = {}) {
  const requestHeaders = new Headers(headers);
  if (owner) requestHeaders.set("X-Share-Owner", owner);
  if (ip) requestHeaders.set("X-Share-IP", ip);
  return new Request(`${origin}${path}`, { method, headers: requestHeaders, body });
}

function createRequest(shareId, options = {}) {
  const expiresAt = options.expiresAt ?? new Date(initialNow + 7 * 24 * 60 * 60 * 1000).toISOString();
  return internalRequest(`/api/shares/${shareId}`, {
    method: "POST",
    owner: options.owner ?? ownerA,
    ip: options.ip ?? ipA,
    body: JSON.stringify({ url: `${origin}/s/${shareId}/`, expiresAt }),
  });
}

async function create(service, shareId, options) {
  return service.fetch(createRequest(shareId, options));
}

async function upload(service, shareId, path, body, owner = ownerA, headers = {}) {
  return service.fetch(internalRequest(`/api/shares/${shareId}/${path}`, { method: "PUT", owner, body, headers }));
}

describe("authentication and ownership", () => {
  test("the public handler rejects malformed secrets and hashes valid ones", async () => {
    const state = fixture();
    const namespace = {
      idFromName(name) { return name; },
      get() { return { fetch: (request) => state.service.fetch(request) }; },
    };
    const shareId = id(1);
    const payload = JSON.stringify({ url: `${origin}/s/${shareId}/`, expiresAt: new Date(initialNow + 1000).toISOString() });
    const invalid = await worker.fetch(new Request(`${origin}/api/shares/${shareId}`, {
      method: "POST",
      headers: { Authorization: "Bearer ABC", "CF-Connecting-IP": "192.0.2.1" },
      body: payload,
    }), { COORDINATOR: namespace });
    expect(invalid.status).toBe(401);

    const secret = "c".repeat(64);
    const valid = await worker.fetch(new Request(`${origin}/api/shares/${shareId}`, {
      method: "POST",
      headers: { Authorization: `Bearer ${secret}`, "CF-Connecting-IP": "192.0.2.1" },
      body: payload,
    }), { COORDINATOR: namespace });
    expect(valid.status).toBe(201);
    expect(state.metadata.getShare(shareId).owner_hash).toBe(await sha256Hex(secret));
    expect(JSON.stringify(shareRows(state.metadata))).not.toContain(secret);
    expect(state.metadata.getShare(shareId).ip_hash).toBe(await sha256Hex("192.0.2.1"));
  });

  test("another installation cannot upload, complete, or delete", async () => {
    const { service } = fixture();
    const shareId = id(2);
    expect((await create(service, shareId)).status).toBe(201);
    expect((await upload(service, shareId, "take.md", "text", ownerB)).status).toBe(403);
    expect((await service.fetch(internalRequest(`/api/shares/${shareId}/complete`, { method: "POST", owner: ownerB }))).status).toBe(403);
    expect((await service.fetch(internalRequest(`/api/shares/${shareId}`, { method: "DELETE", owner: ownerB }))).status).toBe(403);
  });
});

describe("creation quotas", () => {
  test("empty app requests generate expiry once and accept native segment directories", async () => {
    const { service, setNow } = fixture();
    const shareId = id(99);
    const make = () => internalRequest(`/api/shares/${shareId}`, { method: "POST" });
    const first = await service.fetch(make());
    expect(first.status).toBe(201);
    const initial = await first.json();
    setNow(initialNow + 60_000);
    const retry = await service.fetch(make());
    expect(retry.status).toBe(200);
    expect((await retry.json()).expiresAt).toBe(initial.expiresAt);
    expect((await upload(service, shareId, "live/2026-09-09-01.segments/init.mp4", "init")).status).toBe(201);
    expect(validateRelativePath("live/../init.mp4")).toBeNull();
  });
  test("same-ID retries are free and conflicting retries fail", async () => {
    const { service, metadata } = fixture();
    const shareId = id(3);
    expect((await create(service, shareId)).status).toBe(201);
    expect((await create(service, shareId)).status).toBe(200);
    expect(shareRows(metadata)).toHaveLength(1);
    expect((await create(service, shareId, { owner: ownerB })).status).toBe(409);
  });

  test("installation and IP limits apply before inserting", async () => {
    const first = fixture();
    for (let index = 0; index < 10; index += 1) expect((await create(first.service, id(100 + index), { ip: index.toString(16).padStart(64, "0") })).status).toBe(201);
    expect((await create(first.service, id(111), { ip: "f".repeat(64) })).status).toBe(429);
    expect(shareRows(first.metadata)).toHaveLength(10);

    const second = fixture();
    for (let index = 0; index < 20; index += 1) {
      const owner = (index + 1).toString(16).padStart(64, "0");
      expect((await create(second.service, id(200 + index), { owner })).status).toBe(201);
    }
    const response = await create(second.service, id(221), { owner: "f".repeat(64) });
    expect(response.status).toBe(429);
    expect((await response.json()).error).toBe("ip_day");
  });

  test("100 concurrent creations cannot exceed the global daily cap", async () => {
    const { service, metadata } = fixture();
    const responses = await Promise.all(Array.from({ length: 112 }, (_, index) => create(service, id(1000 + index), {
      owner: (1000 + index).toString(16).padStart(64, "0"),
      ip: (2000 + index).toString(16).padStart(64, "0"),
    })));
    expect(responses.filter((response) => response.status === 201)).toHaveLength(100);
    expect(responses.filter((response) => response.status === 429)).toHaveLength(12);
    expect(shareRows(metadata)).toHaveLength(100);
  });
});

describe("uploads", () => {
  test("replacement accounting uses the byte delta and same-content retries are stable", async () => {
    const { service, metadata } = fixture();
    const shareId = id(4);
    await create(service, shareId);
    expect((await upload(service, shareId, "shots/001.jpg", "12345")).status).toBe(201);
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: 5, file_count: 1 });
    expect((await upload(service, shareId, "shots/001.jpg", "123456789")).status).toBe(201);
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: 9, file_count: 1 });
    expect((await upload(service, shareId, "shots/001.jpg", "123456789")).status).toBe(201);
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: 9, file_count: 1 });
    expect((await upload(service, shareId, "shots/001.jpg", "12")).status).toBe(201);
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: 2, file_count: 1 });
  });

  test("failed R2 writes roll back reservations", async () => {
    const { service, metadata, bucket } = fixture();
    const shareId = id(5);
    await create(service, shareId);
    bucket.failPut = true;
    expect((await upload(service, shareId, "take.md", "hello")).status).toBe(502);
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: 0, file_count: 0 });
    expect(metadata.getFile(shareId, "take.md")).toBeNull();
  });

  test("a retry reconciles a reservation left between SQLite and R2", async () => {
    const { service, metadata, bucket } = fixture();
    const shareId = id(51);
    await create(service, shareId);
    const bytes = new TextEncoder().encode("first");
    const sha = await sha256Hex(bytes);
    expect(metadata.reserveUpload(shareId, "take.md", bytes.byteLength, sha, initialNow)).toMatchObject({ kind: "reserved" });
    await bucket.put(`${shareId}/take.md`, bytes, { customMetadata: { sha256: sha } });
    expect(metadata.getFile(shareId, "take.md").committed_sha).toBeNull();

    expect((await upload(service, shareId, "transcript.json", "{}")).status).toBe(201);
    expect(metadata.getFile(shareId, "take.md")).toMatchObject({ committed_sha: sha, pending_sha: null });
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: 7, file_count: 2 });
  });

  test("share byte and file-count limits reject before R2 mutation", async () => {
    const first = fixture();
    const byteLimitedId = id(52);
    await create(first.service, byteLimitedId);
    first.metadata.sql.exec("UPDATE shares SET total_bytes = ? WHERE id = ?", LIMITS.shareBytes, byteLimitedId);
    const byteResponse = await upload(first.service, byteLimitedId, "extra.json", "{}");
    expect(byteResponse.status).toBe(413);
    expect((await byteResponse.json()).error).toBe("share_bytes");
    expect(first.bucket.objects.size).toBe(0);

    const second = fixture();
    const fileLimitedId = id(53);
    await create(second.service, fileLimitedId);
    second.metadata.sql.exec("UPDATE shares SET file_count = ? WHERE id = ?", LIMITS.shareFiles, fileLimitedId);
    const fileResponse = await upload(second.service, fileLimitedId, "extra.json", "{}");
    expect(fileResponse.status).toBe(413);
    expect((await fileResponse.json()).error).toBe("share_files");
    expect(second.bucket.objects.size).toBe(0);
  });

  test("chunked bodies are counted and oversized or misleading bodies fail before accounting", async () => {
    const { service, metadata } = fixture();
    const shareId = id(6);
    await create(service, shareId);
    const chunks = [new Uint8Array(LIMITS.objectBytes), new Uint8Array([1])];
    const stream = new ReadableStream({
      pull(controller) {
        const chunk = chunks.shift();
        if (chunk) controller.enqueue(chunk);
        else controller.close();
      },
    });
    expect((await upload(service, shareId, "fullclip.mp4", stream)).status).toBe(413);
    expect((await upload(service, shareId, "take.md", "longer", ownerA, { "Content-Length": "2" })).status).toBe(413);
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: 0, file_count: 0 });
  });

  test("path traversal, controls, encoded paths, and unknown types fail", async () => {
    for (const path of ["../take.md", "shots/%2e%2e/take.md", "shots/a.exe", "shots/a.html", "shots\\a.jpg", `shots/${"a".repeat(125)}.jpg`]) {
      expect(validateRelativePath(path)).toBeNull();
    }
    expect(validateRelativePath("live/001/index.m3u8")).toMatchObject({ contentType: "application/vnd.apple.mpegurl" });
    expect(validateRelativePath("clips/001/clip.mov")).toMatchObject({ contentType: "video/quicktime" });
  });
});

describe("concurrent shares", () => {
  test("a stalled storage write leaves another owner free to create, upload, publish and read", async () => {
    const { service, bucket, metadata } = fixture();
    const [slowShare, otherShare] = [id(300), id(301)];
    await create(service, slowShare);
    const stalled = bucket.hold("put", `${slowShare}/shots/001.jpg`);
    const slow = upload(service, slowShare, "shots/001.jpg", "slow");
    await stalled.reached;

    expect((await create(service, otherShare, { owner: ownerB, ip: ipB })).status).toBe(201);
    expect((await upload(service, otherShare, "transcript.json", "{}", ownerB)).status).toBe(201);
    expect((await upload(service, otherShare, "take.md", "# Take", ownerB)).status).toBe(201);
    expect((await service.fetch(internalRequest(`/api/shares/${otherShare}/complete`, { method: "POST", owner: ownerB }))).status).toBe(200);
    expect(await (await service.fetch(new Request(`${origin}/s/${otherShare}/take.md`))).text()).toBe("# Take");
    expect(await settled(slow)).toBe(false);

    stalled.release();
    expect((await slow).status).toBe(201);
    expect(metadata.getShare(slowShare)).toMatchObject({ state: "open", total_bytes: 4, file_count: 1 });
    expect(metadata.getShare(otherShare)).toMatchObject({ state: "ready", total_bytes: 8, file_count: 2 });
  });

  test("creation limits hold while another share's write is stalled", async () => {
    const { service, bucket, metadata } = fixture();
    await create(service, id(310));
    const stalled = bucket.hold("put", `${id(310)}/take.md`);
    const slow = upload(service, id(310), "take.md", "text");
    await stalled.reached;
    const responses = await Promise.all(Array.from({ length: 12 }, (_, index) => create(service, id(320 + index), {
      ip: (index + 2).toString(16).padStart(64, "0"),
    })));
    expect(responses.filter((response) => response.status === 201)).toHaveLength(LIMITS.installationMonth - 1);
    expect(responses.filter((response) => response.status === 429)).toHaveLength(3);
    stalled.release();
    expect((await slow).status).toBe(201);
    expect(shareRows(metadata)).toHaveLength(LIMITS.installationMonth);
  });

  test("writes to one share keep their order and count each byte once", async () => {
    const { service, bucket, metadata } = fixture();
    const shareId = id(302);
    await create(service, shareId);
    const [older, newer] = ['{"shots":[]}', '{"shots":[{"file":"shots/001.jpg"}]}'];
    const stalled = bucket.hold("put", `${shareId}/live.json`);
    const first = upload(service, shareId, "live.json", older);
    await stalled.reached;
    const retry = upload(service, shareId, "live.json", older);
    const replacement = upload(service, shareId, "live.json", newer);
    const sibling = upload(service, shareId, "shots/001.jpg", "image");
    expect(await settled(Promise.race([retry, replacement, sibling]))).toBe(false);

    stalled.release();
    const responses = await Promise.all([first, retry, replacement, sibling]);
    expect(responses.map((response) => response.status)).toEqual([201, 201, 201, 201]);
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: newer.length + 5, file_count: 2 });
    expect(await (await service.fetch(new Request(`${origin}/s/${shareId}/live.json`))).text()).toBe(newer);
  });

  test("queued writes cannot take a share past its byte limit", async () => {
    const { service, bucket, metadata } = fixture();
    const shareId = id(303);
    await create(service, shareId);
    metadata.sql.exec("UPDATE shares SET total_bytes = ? WHERE id = ?", LIMITS.shareBytes - 5, shareId);
    const stalled = bucket.hold("put", `${shareId}/a.json`);
    const first = upload(service, shareId, "a.json", "1234");
    await stalled.reached;
    const second = upload(service, shareId, "b.json", "1234");
    stalled.release();
    expect((await first).status).toBe(201);
    const rejected = await second;
    expect(rejected.status).toBe(413);
    expect((await rejected.json()).error).toBe("share_bytes");
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: LIMITS.shareBytes - 1, file_count: 1 });
    expect(bucket.objects.has(`${shareId}/b.json`)).toBe(false);
  });

  test("a failed write gives its reserved room to the next queued write", async () => {
    const { service, bucket, metadata } = fixture();
    const shareId = id(304);
    await create(service, shareId);
    metadata.sql.exec("UPDATE shares SET total_bytes = ? WHERE id = ?", LIMITS.shareBytes - 5, shareId);
    const stalled = bucket.hold("put", `${shareId}/a.json`);
    const first = upload(service, shareId, "a.json", "1234");
    await stalled.reached;
    const second = upload(service, shareId, "b.json", "1234");
    stalled.release(new Error("R2 failure"));
    expect((await first).status).toBe(502);
    expect((await second).status).toBe(201);
    expect(metadata.getShare(shareId)).toMatchObject({ total_bytes: LIMITS.shareBytes - 1, file_count: 1 });
    expect(metadata.getFile(shareId, "a.json")).toBeNull();
  });
});

describe("a coordinator restarted over stored state", () => {
  test("keeps its shares, creation counts and unfinished reservations", async () => {
    const earlier = fixture();
    const [ready, canceled, landed, lost] = ["0123456789abcdef0123456789abcdef", id(500), id(501), id(502)];
    for (const shareId of [ready, canceled, landed, lost]) await create(earlier.service, shareId);
    await upload(earlier.service, ready, "transcript.json", "{}");
    await upload(earlier.service, ready, "take.md", "# Take");
    await earlier.service.fetch(internalRequest(`/api/shares/${ready}/complete`, { method: "POST" }));
    await upload(earlier.service, canceled, "take.md", "# Take");
    await earlier.service.fetch(internalRequest(`/api/shares/${canceled}`, { method: "DELETE" }));
    // The instance stops during two writes. Storage kept the first one.
    const kept = earlier.bucket.hold("put", `${landed}/shots/001.jpg`, { after: true });
    const dropped = earlier.bucket.hold("put", `${lost}/shots/001.jpg`);
    upload(earlier.service, landed, "shots/001.jpg", "image");
    upload(earlier.service, lost, "shots/001.jpg", "image");
    await Promise.all([kept.reached, dropped.reached]);

    const { service, metadata } = fixture(earlier.database, earlier.bucket);
    expect(await (await service.fetch(new Request(`${origin}/s/${ready}/take.md`))).text()).toBe("# Take");
    expect((await upload(service, ready, "extra.json", "{}")).status).toBe(409);
    expect((await service.fetch(new Request(`${origin}/s/${canceled}/`))).status).toBe(404);
    expect((await create(service, canceled)).status).toBe(409);

    expect((await upload(service, landed, "live.json", "{}")).status).toBe(201);
    expect(metadata.getShare(landed)).toMatchObject({ total_bytes: 7, file_count: 2 });
    expect(await (await service.fetch(new Request(`${origin}/s/${landed}/shots/001.jpg`))).text()).toBe("image");
    expect((await upload(service, lost, "live.json", "{}")).status).toBe(201);
    expect(metadata.getShare(lost)).toMatchObject({ total_bytes: 2, file_count: 1 });
    expect((await service.fetch(new Request(`${origin}/s/${lost}/shots/001.jpg`))).status).toBe(404);

    const created = [];
    for (let index = 0; index < LIMITS.installationMonth - 3; index += 1) created.push((await create(service, id(510 + index))).status);
    expect(created).toEqual([...Array(LIMITS.installationMonth - 4).fill(201), 429]);
  });
});

describe("completion, cancellation, and expiry", () => {
  test("cancel arriving before create reserves an owner tombstone", async () => {
    const { service } = fixture();
    const shareId = id(200);
    const removed = await service.fetch(internalRequest(`/api/shares/${shareId}`, { method: "DELETE" }));
    expect(removed.status).toBe(204);
    expect((await create(service, shareId)).status).toBe(409);
    expect((await service.fetch(new Request(`${origin}/s/${shareId}/`))).status).toBe(404);
  });
  test("ready requires both durable metadata and R2 objects", async () => {
    const { service, metadata, bucket } = fixture();
    const shareId = id(7);
    await create(service, shareId);
    await upload(service, shareId, "transcript.json", "{}");
    let response = await service.fetch(internalRequest(`/api/shares/${shareId}/complete`, { method: "POST" }));
    expect(response.status).toBe(409);
    expect(metadata.getShare(shareId).state).toBe("open");
    await upload(service, shareId, "take.md", "# Take");
    bucket.objects.delete(`${shareId}/take.md`);
    response = await service.fetch(internalRequest(`/api/shares/${shareId}/complete`, { method: "POST" }));
    expect(response.status).toBe(409);
    expect(metadata.getShare(shareId).state).toBe("open");
    await upload(service, shareId, "take.md", "# Take");
    expect((await service.fetch(internalRequest(`/api/shares/${shareId}/complete`, { method: "POST" }))).status).toBe(200);
    expect((await service.fetch(internalRequest(`/api/shares/${shareId}/complete`, { method: "POST" }))).status).toBe(200);
    expect((await upload(service, shareId, "extra.json", "{}")).status).toBe(409);
  });

  test("a tombstone blocks recreation and public reads after paginated cleanup", async () => {
    const { service, bucket } = fixture();
    const shareId = id(8);
    await create(service, shareId);
    bucket.pageSize = 2;
    await Promise.all(["take.md", "transcript.json", "shots/001.jpg", "shots/002.jpg", "shots/003.jpg"].map((path) => upload(service, shareId, path, "x")));
    expect((await service.fetch(internalRequest(`/api/shares/${shareId}`, { method: "DELETE" }))).status).toBe(204);
    expect([...bucket.objects.keys()].filter((key) => key.startsWith(`${shareId}/`))).toHaveLength(0);
    expect((await service.fetch(new Request(`${origin}/s/${shareId}/`))).status).toBe(404);
    expect((await create(service, shareId)).status).toBe(409);
    expect((await service.fetch(internalRequest(`/api/shares/${shareId}`, { method: "DELETE" }))).status).toBe(204);
  });

  test("a cancel during a stalled write takes the link down at once and leaves no object", async () => {
    const { service, bucket, metadata } = fixture();
    const shareId = id(9);
    await create(service, shareId);
    await upload(service, shareId, "take.md", "# Take");
    const stalled = bucket.hold("put", `${shareId}/shots/001.jpg`);
    const putting = upload(service, shareId, "shots/001.jpg", "image");
    await stalled.reached;
    const deleting = service.fetch(internalRequest(`/api/shares/${shareId}`, { method: "DELETE" }));
    expect((await service.fetch(new Request(`${origin}/s/${shareId}/`))).status).toBe(404);
    expect((await service.fetch(new Request(`${origin}/s/${shareId}/take.md`))).status).toBe(404);
    expect((await upload(service, shareId, "shots/002.jpg", "image")).status).toBe(409);
    stalled.release();
    const late = await putting;
    expect(late.status).toBe(409);
    expect((await late.json()).error).toBe("share_deleted");
    expect((await deleting).status).toBe(204);
    expect([...bucket.objects.keys()].filter((key) => key.startsWith(`${shareId}/`))).toHaveLength(0);
    expect(metadata.getFile(shareId, "shots/001.jpg")).toBeNull();
  });

  test("a write that lands after the share expired is refused and not counted", async () => {
    const state = fixture();
    const shareId = id(14);
    await create(state.service, shareId, { expiresAt: new Date(initialNow + 1000).toISOString() });
    const stalled = state.bucket.hold("put", `${shareId}/take.md`);
    const putting = upload(state.service, shareId, "take.md", "text");
    await stalled.reached;
    state.setNow(initialNow + 1001);
    stalled.release();
    expect((await putting).status).toBe(410);
    expect(state.metadata.getShare(shareId)).toMatchObject({ total_bytes: 0, file_count: 0 });
    expect(state.metadata.getFile(shareId, "take.md")).toBeNull();
  });

  test("completion waits for a write still in flight on its own share", async () => {
    const { service, bucket, metadata } = fixture();
    const shareId = id(15);
    await create(service, shareId);
    await upload(service, shareId, "transcript.json", "{}");
    const stalled = bucket.hold("put", `${shareId}/take.md`);
    const putting = upload(service, shareId, "take.md", "# Take");
    await stalled.reached;
    const completing = service.fetch(internalRequest(`/api/shares/${shareId}/complete`, { method: "POST" }));
    expect(await settled(completing)).toBe(false);
    stalled.release();
    expect((await putting).status).toBe(201);
    expect((await completing).status).toBe(200);
    expect(metadata.getShare(shareId).state).toBe("ready");
  });

  test("expired shares are private and reject further writes", async () => {
    const state = fixture();
    const shareId = id(10);
    await create(state.service, shareId, { expiresAt: new Date(initialNow + 1000).toISOString() });
    await upload(state.service, shareId, "take.md", "text");
    state.setNow(initialNow + 1001);
    expect((await state.service.fetch(new Request(`${origin}/s/${shareId}/`))).status).toBe(404);
    expect((await state.service.fetch(new Request(`${origin}/s/${shareId}/take.md`))).status).toBe(404);
    expect((await upload(state.service, shareId, "more.md", "text")).status).toBe(410);
  });
});

describe("public reads", () => {
  test("files support HEAD, conditional GET, and valid or invalid byte ranges", async () => {
    const { service } = fixture();
    const shareId = id(11);
    await create(service, shareId);
    await upload(service, shareId, "fullclip.mp4", "abcdefghij");
    const url = `${origin}/s/${shareId}/fullclip.mp4`;
    const head = await service.fetch(new Request(url, { method: "HEAD" }));
    expect(head.status).toBe(200);
    expect(head.headers.get("Content-Length")).toBe("10");
    expect(await head.text()).toBe("");
    const etag = head.headers.get("ETag");
    expect((await service.fetch(new Request(url, { headers: { "If-None-Match": etag } }))).status).toBe(304);
    for (const [range, expected, contentRange] of [
      ["bytes=2-5", "cdef", "bytes 2-5/10"],
      ["bytes=-3", "hij", "bytes 7-9/10"],
      ["bytes=8-", "ij", "bytes 8-9/10"],
    ]) {
      const response = await service.fetch(new Request(url, { headers: { Range: range } }));
      expect(response.status).toBe(206);
      expect(response.headers.get("Content-Range")).toBe(contentRange);
      expect(await response.text()).toBe(expected);
    }
    const invalid = await service.fetch(new Request(url, { headers: { Range: "bytes=20-30" } }));
    expect(invalid.status).toBe(416);
    expect(invalid.headers.get("Content-Range")).toBe("bytes */10");
  });

  test("status and viewer stay available while uploading without unsafe DOM sinks", async () => {
    const { service } = fixture();
    const shareId = id(12);
    await create(service, shareId);
    const status = await service.fetch(new Request(`${origin}/s/${shareId}/status.json`));
    expect(status.headers.get("Cache-Control")).toBe("no-store");
    expect(await status.json()).toMatchObject({ state: "uploading" });
    const page = await service.fetch(new Request(`${origin}/s/${shareId}/`));
    const html = await page.text();
    expect(html).not.toContain("innerHTML");
    expect(html).not.toContain("outerHTML");
    expect(html).toContain("textContent");
    expect(page.headers.get("Content-Security-Policy")).toContain("script-src 'nonce-");
    expect(viewerHtml("fixed-nonce")).not.toContain("https://cdn");
  });

  test("absolute Markdown rewrites only allowlisted relative assets", () => {
    const share = `${origin}/s/${id(13)}/`;
    const source = [
      "![safe](shots/001.jpg)",
      "[transcript](transcript.json)",
      "[site](https://example.com/file.jpg)",
      "[script](javascript:alert(1))",
      "[parent](../secret.md)",
      "[local](/Users/person/take.md)",
    ].join("\n");
    const rewritten = rewriteMarkdownPaths(source, share);
    expect(rewritten).toContain(`![safe](${share}shots/001.jpg)`);
    expect(rewritten).toContain(`[transcript](${share}transcript.json)`);
    expect(rewritten).toContain("[site](https://example.com/file.jpg)");
    expect(rewritten).toContain("[script](javascript:alert(1))");
    expect(rewritten).toContain("[parent](../secret.md)");
    expect(rewritten).toContain("[local](/Users/person/take.md)");
  });

  test("range parser rejects multiple and zero-length suffix ranges", () => {
    expect(parseByteRange("bytes=0-1,3-4", 10)).toBe(false);
    expect(parseByteRange("bytes=-0", 10)).toBe(false);
    expect(parseByteRange("bytes=0-99", 10)).toEqual({ offset: 0, length: 10, end: 9 });
  });
});
