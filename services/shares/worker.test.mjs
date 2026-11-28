import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import vm from "node:vm";
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

function fixture(database = new Database(":memory:"), bucket = new MemoryR2()) {
  let now = initialNow;
  const metadata = new SqliteMetadata(storage(database));
  const service = new ShareCoordinatorService(metadata, bucket, { now: () => now, publicOrigin: origin });
  return { database, metadata, bucket, service, setNow(value) { now = value; } };
}

function shareRows(metadata) {
  return metadata.sql.exec("SELECT * FROM shares").toArray();
}

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

  test("a file replaced during a read is served as one version", async () => {
    const [older, newer] = ['{"shots":[]}', '{"shots":[{"file":"shots/001.jpg"}]}'];
    for (const readFirst of [false, true]) {
      const { service, bucket } = fixture();
      const shareId = id(16);
      await create(service, shareId);
      await upload(service, shareId, "live.json", older);
      const reading = bucket.hold("get", `${shareId}/live.json`, { after: readFirst });
      const pending = service.fetch(new Request(`${origin}/s/${shareId}/live.json`));
      await reading.reached;
      expect((await upload(service, shareId, "live.json", newer)).status).toBe(201);
      reading.release();
      const response = await pending;
      const body = await response.text();
      expect(response.status).toBe(200);
      expect(body).toBe(readFirst ? older : newer);
      expect(response.headers.get("Content-Length")).toBe(String(body.length));
      expect(response.headers.get("ETag")).toBe(`"${await sha256Hex(body)}"`);
    }
  });

  test("ranges and validators follow a replaced file", async () => {
    const { service } = fixture();
    const shareId = id(17);
    await create(service, shareId);
    const url = `${origin}/s/${shareId}/live/001/index.m3u8`;
    await upload(service, shareId, "live/001/index.m3u8", "#EXTM3U\n#EXTINF:1,\nsegment-000.m4s\n");
    const stale = (await service.fetch(new Request(url, { method: "HEAD" }))).headers.get("ETag");
    const playlist = "#EXTM3U\n#EXTINF:1,\nsegment-000.m4s\n#EXTINF:1,\nsegment-001.m4s\n";
    await upload(service, shareId, "live/001/index.m3u8", playlist);

    const head = await service.fetch(new Request(url, { method: "HEAD" }));
    expect(head.headers.get("Content-Length")).toBe(String(playlist.length));
    const fresh = head.headers.get("ETag");
    expect(fresh).not.toBe(stale);
    const changed = await service.fetch(new Request(url, { headers: { "If-None-Match": stale } }));
    expect(changed.status).toBe(200);
    expect(await changed.text()).toBe(playlist);
    expect((await service.fetch(new Request(url, { headers: { "If-None-Match": fresh } }))).status).toBe(304);
    const tail = await service.fetch(new Request(url, { headers: { Range: "bytes=-16" } }));
    expect(tail.status).toBe(206);
    expect(tail.headers.get("Content-Range")).toBe(`bytes ${playlist.length - 16}-${playlist.length - 1}/${playlist.length}`);
    expect(tail.headers.get("Content-Length")).toBe("16");
    expect(await tail.text()).toBe(playlist.slice(-16));
    const invalid = await service.fetch(new Request(url, { headers: { Range: `bytes=${playlist.length}-` } }));
    expect(invalid.status).toBe(416);
    expect(invalid.headers.get("Content-Range")).toBe(`bytes */${playlist.length}`);
  });

  test("a read in flight when the share is canceled is not served", async () => {
    const { service, bucket } = fixture();
    const shareId = id(18);
    await create(service, shareId);
    await upload(service, shareId, "take.md", "# Take");
    const reading = bucket.hold("get", `${shareId}/take.md`, { after: true });
    const pending = service.fetch(new Request(`${origin}/s/${shareId}/take.md`));
    await reading.reached;
    const deleting = service.fetch(internalRequest(`/api/shares/${shareId}`, { method: "DELETE" }));
    reading.release();
    expect((await pending).status).toBe(404);
    expect((await deleting).status).toBe(204);
  });

  test("absolute Markdown is built from the committed take.md", async () => {
    const { service, bucket } = fixture();
    const shareId = id(19);
    await create(service, shareId);
    await upload(service, shareId, "take.md", "![shot](shots/001.jpg)");
    const url = `${origin}/s/${shareId}/take.md?absolute=1`;
    const absolute = `![shot](${origin}/s/${shareId}/shots/001.jpg)`;
    const full = await service.fetch(new Request(url));
    expect(await full.text()).toBe(absolute);
    expect(full.headers.get("Content-Length")).toBe(String(absolute.length));
    const head = await service.fetch(new Request(url, { method: "HEAD" }));
    expect(head.headers.get("ETag")).toBe(full.headers.get("ETag"));
    expect(await (await service.fetch(new Request(url, { headers: { Range: "bytes=0-7" } }))).text()).toBe("![shot](");

    const reading = bucket.hold("get", `${shareId}/take.md`);
    const pending = service.fetch(new Request(url));
    await reading.reached;
    await bucket.put(`${shareId}/take.md`, new TextEncoder().encode("[unfinished](shots/002.jpg)"));
    reading.release();
    expect((await pending).status).toBe(404);
  });

  test("first-beta shares keep their 32-character IDs, packaged video and ranges", async () => {
    const { service } = fixture();
    const legacy = "0123456789abcdef0123456789abcdef";
    expect((await create(service, legacy)).status).toBe(201);
    await upload(service, legacy, "fullclip.mp4", "abcdefghij");
    await upload(service, legacy, "clips/001/clip.mov", "quicktime");
    expect((await service.fetch(new Request(`${origin}/s/${legacy}/`))).status).toBe(200);
    const probe = await service.fetch(new Request(`${origin}/s/${legacy}/fullclip.mp4`, { method: "HEAD" }));
    expect(probe.status).toBe(200);
    expect(probe.headers.get("Content-Length")).toBe("10");
    const part = await service.fetch(new Request(`${origin}/s/${legacy}/fullclip.mp4`, { headers: { Range: "bytes=0-1" } }));
    expect(part.status).toBe(206);
    expect(await part.text()).toBe("ab");
    const movie = await service.fetch(new Request(`${origin}/s/${legacy}/clips/001/clip.mov`));
    expect(movie.headers.get("Content-Type")).toBe("video/quicktime");
    expect(await movie.text()).toBe("quicktime");
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

// The DOM calls the viewer makes. A node counts each time it leaves its
// parent, which is what interrupts a playing video in a browser.
class PageNode {
  constructor(name, page) {
    this.name = name;
    this.page = page;
    this.children = [];
    this.parent = null;
    this.detached = 0;
    this.listeners = {};
    this.paused = true;
    this.plays = 0;
  }

  get firstChild() {
    return this.children[0] ?? null;
  }

  get nextSibling() {
    return this.parent.children[this.parent.children.indexOf(this) + 1] ?? null;
  }

  append(...nodes) {
    for (const node of nodes) this.insertBefore(node, null);
  }

  insertBefore(node, before) {
    node.remove();
    this.children.splice(before ? this.children.indexOf(before) : this.children.length, 0, node);
    node.parent = this;
    if (node.name === "script") this.page.loading.push(node);
  }

  remove() {
    if (!this.parent) return;
    this.parent.children.splice(this.parent.children.indexOf(this), 1);
    this.parent = null;
    this.detached += 1;
  }

  replaceWith(node) {
    this.parent.insertBefore(node, this);
    this.remove();
  }

  canPlayType() {
    return this.page.nativeHls ? "maybe" : "";
  }

  addEventListener(type, listener) {
    this.listeners[type] = listener;
  }

  removeEventListener(type) {
    delete this.listeners[type];
  }

  play() {
    this.paused = false;
    this.plays += 1;
    this.listeners.play?.();
    return Promise.resolve();
  }

  // Chrome pauses a video whose source failed before it reports the error.
  fail(code) {
    this.paused = true;
    this.error = { code };
    this.listeners.error?.();
    this.listeners.pause();
  }
}

const viewerRoot = `${origin}/s/${id(1)}/`;

async function openViewer({ responses, nativeHls = false, playerLoads = true }) {
  const source = /<script nonce="nonce">([\s\S]*?)<\/script>/.exec(viewerHtml("nonce"))[1];
  const page = { responses, nativeHls, requests: [], timers: [], players: [], loading: [] };
  const node = (name) => new PageNode(name, page);
  Object.assign(page, { status: node("p"), narration: node("p"), captures: node("div"), head: node("head") });
  class Hls {
    static isSupported() {
      return true;
    }

    constructor() {
      page.players.push(this);
    }

    loadSource(url) {
      this.url = url;
    }

    attachMedia(video) {
      this.video = video;
    }

    destroy() {
      this.destroyed = true;
    }
  }
  const context = vm.createContext({
    URL,
    location: { href: viewerRoot },
    document: {
      head: page.head,
      querySelector: (selector) => ({ "#status": page.status, "#narration": page.narration, "#captures": page.captures })[selector],
      createElement: node,
    },
    setTimeout: (callback) => page.timers.push(callback),
    async fetch(target, options = {}) {
      const path = String(target).replace(viewerRoot, "");
      page.requests.push(`${options.method ?? "GET"} ${path}`);
      const body = await page.responses[path];
      return { ok: body !== undefined, json: async () => body };
    },
  });
  page.settle = async () => {
    for (let turn = 0; turn < 5; turn += 1) {
      for (const script of page.loading.splice(0)) {
        if (!playerLoads) script.onerror();
        else {
          context.Hls = Hls;
          script.onload();
        }
      }
      await new Promise((resolve) => setImmediate(resolve));
    }
  };
  page.poll = async () => {
    await page.timers.shift()();
    await page.settle();
  };
  page.captions = () => page.captures.children.map((figure) => figure.children[1].textContent ?? figure.children[1].children[0].textContent);
  vm.runInContext(source, context);
  await page.settle();
  return page;
}

describe("viewer page", () => {
  const uploading = { state: "uploading", expiresAt: new Date(initialNow + LIMITS.retentionMs).toISOString() };
  const playlist = "live/2026-09-09-01.segments/index.m3u8";
  const laterPlaylist = "live/2026-09-09-02.segments/index.m3u8";

  test("screenshots and browsers that play HLS themselves never fetch the player", async () => {
    expect(viewerHtml("nonce").match(/<script/g)).toHaveLength(1);
    const shots = await openViewer({ responses: { "status.json": uploading, "live.json": { clips: [], shots: [{ file: "shots/001.jpg" }] } } });
    expect(shots.captures.children.map((figure) => figure.children[0].src)).toEqual([`${viewerRoot}shots/001.jpg`]);
    expect(shots.head.children).toHaveLength(0);

    const native = await openViewer({ nativeHls: true, responses: { "status.json": uploading, "live.json": { clips: [{ playlist }], shots: [] } } });
    expect(native.captures.children.map((figure) => figure.children[0].src)).toEqual([`${viewerRoot}${playlist}`]);
    expect(native.head.children).toHaveLength(0);
  });

  test("a new screenshot leaves a clip and its player in place", async () => {
    const responses = { "status.json": uploading, "live.json": { clips: [{ playlist }, { playlist: laterPlaylist }], shots: [] } };
    const page = await openViewer({ responses });
    expect(page.head.children.map((script) => [script.src, script.nonce])).toEqual([["/hls-1.7.2.min.js", "nonce"]]);
    const [clip, laterClip] = page.captures.children;
    expect(page.players.map((player) => [player.url, player.video])).toEqual([
      [`${viewerRoot}${playlist}`, clip.children[0]],
      [`${viewerRoot}${laterPlaylist}`, laterClip.children[0]],
    ]);

    responses["live.json"] = { clips: [{ playlist }, { playlist: laterPlaylist }], shots: [{ file: "shots/001.jpg" }] };
    await page.poll();
    expect(page.captions()).toEqual(["Screenshot recorded during upload", "Live video clip", "Live video clip"]);
    expect(page.captures.children.slice(1)).toEqual([clip, laterClip]);
    expect([clip.detached, laterClip.detached]).toEqual([0, 0]);
    expect(page.players.map((player) => player.destroyed)).toEqual([undefined, undefined]);
    expect(page.head.children).toHaveLength(1);

    responses["live.json"] = { clips: [{ playlist }], shots: [{ file: "shots/001.jpg" }] };
    await page.poll();
    expect(page.captures.children[1]).toBe(clip);
    expect(page.players.map((player) => player.destroyed)).toEqual([undefined, true]);
  });

  test("the finished take appears before the packaged-video probe answers", async () => {
    for (const packaged of [true, false]) {
      let answer;
      const responses = {
        "status.json": uploading,
        "live.json": { clips: [{ playlist }], shots: [] },
        "transcript.json": {
          fullText: "Look at the sidebar.",
          captures: [
            { type: "clip", video: playlist, durationMs: 12000, frames: [{ file: "clips/001/frames/0004500.jpg", timestamp: "00:04" }] },
            { type: "shot", file: "shots/001.jpg", timestamp: "00:14" },
          ],
        },
        "fullclip.mp4": new Promise((resolve) => { answer = resolve; }),
      };
      const page = await openViewer({ responses });
      const [clip] = page.captures.children;
      responses["status.json"] = { ...uploading, state: "ready" };
      await page.poll();
      const finished = ["Video clip, 12 seconds", "Video frame at 00:04", "Screenshot at 00:14"];
      expect(page.requests).toContain("HEAD fullclip.mp4");
      expect(page.narration.textContent).toBe("Look at the sidebar.");
      expect(page.captions()).toEqual(finished);
      expect(page.captures.children[0]).toBe(clip);

      answer(packaged ? {} : undefined);
      await page.settle();
      expect(page.captions()).toEqual(packaged ? ["Packaged video", ...finished] : finished);
      expect(clip.detached).toBe(0);
      expect(page.players.map((player) => player.destroyed)).toEqual([undefined]);
      expect(page.timers).toHaveLength(0);
    }
  });

  test("a browser whose own HLS playback fails hands the clip to the player", async () => {
    for (const pressedPlay of [false, true]) {
      const page = await openViewer({ nativeHls: true, responses: { "status.json": uploading, "live.json": { clips: [{ playlist }], shots: [] } } });
      const video = page.captures.children[0].children[0];
      expect(page.head.children).toHaveLength(0);
      if (pressedPlay) video.play();
      video.fail(2);
      await page.settle();
      expect(page.head.children).toHaveLength(0);
      if (pressedPlay) video.play();
      video.fail(4);
      video.fail(4);
      await page.settle();
      expect(page.head.children.map((script) => script.src)).toEqual(["/hls-1.7.2.min.js"]);
      expect(page.players.map((player) => [player.url, player.video])).toEqual([[`${viewerRoot}${playlist}`, video]]);
      expect(page.captures.children[0].children[0]).toBe(video);
      expect(video.plays).toBe(pressedPlay ? 3 : 0);
    }
  });

  test("a player that cannot load leaves the link to the clip", async () => {
    const page = await openViewer({ playerLoads: false, responses: { "status.json": uploading, "live.json": { clips: [{ playlist }], shots: [] } } });
    const [figure] = page.captures.children;
    expect(figure.children[0].textContent).toBe("This browser cannot play the clip. Open its link in a media player.");
    expect(figure.children[1].children[0].href).toBe(`${viewerRoot}${playlist}`);
  });
});
