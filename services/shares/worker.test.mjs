import { describe, expect, test } from "bun:test";
import worker, {
  LIMITS,
  ShareCoordinatorService,
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

class MemoryMetadata {
  constructor() {
    this.shares = new Map();
    this.files = new Map();
  }

  fileKey(id, path) {
    return `${id}\n${path}`;
  }

  getShare(id) {
    return this.shares.get(id) ?? null;
  }

  getFile(id, path) {
    return this.files.get(this.fileKey(id, path)) ?? null;
  }

  pendingFiles(id) {
    return [...this.files.values()].filter((file) => file.share_id === id && file.pending_sha !== null);
  }

  createShare(input) {
    const existing = this.getShare(input.id);
    if (existing) return { kind: "existing", share: existing };
    const shares = [...this.shares.values()];
    if (shares.filter((share) => share.owner_hash === input.ownerHash && share.created_at >= input.monthStart).length >= LIMITS.installationMonth) {
      return { kind: "quota", quota: "installation_month" };
    }
    if (shares.filter((share) => share.created_at >= input.dayStart).length >= LIMITS.globalDay) return { kind: "quota", quota: "global_day" };
    if (shares.filter((share) => share.ip_hash === input.ipHash && share.created_at >= input.dayStart).length >= LIMITS.ipDay) {
      return { kind: "quota", quota: "ip_day" };
    }
    const share = {
      id: input.id,
      owner_hash: input.ownerHash,
      ip_hash: input.ipHash,
      url: input.url,
      expires_at: input.expiresAt,
      created_at: input.now,
      state: "open",
      total_bytes: 0,
      file_count: 0,
    };
    this.shares.set(input.id, share);
    return { kind: "created", share };
  }

  reserveUpload(id, path, size, sha, now) {
    const share = this.getShare(id);
    if (!share) return { kind: "missing" };
    if (share.state !== "open") return { kind: "state", state: share.state };
    if (share.expires_at <= now) return { kind: "expired" };
    const key = this.fileKey(id, path);
    const file = this.files.get(key);
    if (file?.pending_sha) return { kind: "pending", file };
    const oldSize = file?.committed_size ?? 0;
    const delta = size - oldSize;
    const isNew = !file || file.committed_size === null;
    if (share.total_bytes + delta > LIMITS.shareBytes) return { kind: "quota", quota: "share_bytes" };
    if (share.file_count + (isNew ? 1 : 0) > LIMITS.shareFiles) return { kind: "quota", quota: "share_files" };
    share.total_bytes += delta;
    if (isNew) share.file_count += 1;
    if (file) {
      file.pending_size = size;
      file.pending_sha = sha;
    } else {
      this.files.set(key, {
        share_id: id,
        path,
        committed_size: null,
        committed_sha: null,
        pending_size: size,
        pending_sha: sha,
      });
    }
    return { kind: "reserved" };
  }

  finalizeUpload(id, path, sha) {
    const file = this.getFile(id, path);
    if (!file || file.pending_sha !== sha) return false;
    file.committed_size = file.pending_size;
    file.committed_sha = file.pending_sha;
    file.pending_size = null;
    file.pending_sha = null;
    return true;
  }

  rollbackUpload(id, path, sha) {
    const key = this.fileKey(id, path);
    const file = this.files.get(key);
    if (!file || file.pending_sha !== sha) return false;
    const share = this.getShare(id);
    share.total_bytes -= file.pending_size - (file.committed_size ?? 0);
    if (file.committed_size === null) {
      share.file_count -= 1;
      this.files.delete(key);
    } else {
      file.pending_size = null;
      file.pending_sha = null;
    }
    return true;
  }

  markReady(id, ownerHash, now) {
    const share = this.getShare(id);
    if (!share) return { kind: "missing" };
    if (share.owner_hash !== ownerHash) return { kind: "owner" };
    if (share.state === "deleted") return { kind: "state", state: "deleted" };
    if (share.expires_at <= now) return { kind: "expired" };
    if (share.state === "ready") return { kind: "ready", share };
    if (!this.getFile(id, "transcript.json")?.committed_sha || !this.getFile(id, "take.md")?.committed_sha) return { kind: "incomplete" };
    share.state = "ready";
    return { kind: "ready", share };
  }

  markDeleted(id, ownerHash) {
    const share = this.getShare(id);
    if (!share) return { kind: "missing" };
    if (share.owner_hash !== ownerHash) return { kind: "owner" };
    share.state = "deleted";
    return { kind: "deleted" };
  }
}

class MemoryR2 {
  constructor() {
    this.objects = new Map();
    this.pageSize = 1000;
    this.putGate = null;
    this.onPutStarted = null;
    this.failPut = false;
  }

  async put(key, value, options = {}) {
    if (this.onPutStarted) this.onPutStarted();
    if (this.putGate) await this.putGate;
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
  }

  async head(key) {
    const value = this.objects.get(key);
    return value ? { size: value.size, httpEtag: value.httpEtag, customMetadata: value.customMetadata, httpMetadata: value.httpMetadata } : null;
  }

  async get(key, options = {}) {
    const value = this.objects.get(key);
    if (!value) return null;
    const range = options.range;
    return { body: range ? value.bytes.slice(range.offset, range.offset + range.length) : value.bytes.slice() };
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

function fixture() {
  let now = initialNow;
  const metadata = new MemoryMetadata();
  const bucket = new MemoryR2();
  const service = new ShareCoordinatorService(metadata, bucket, { now: () => now, publicOrigin: origin });
  return { metadata, bucket, service, setNow(value) { now = value; } };
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
    expect(JSON.stringify([...state.metadata.shares.values()])).not.toContain(secret);
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
    expect(metadata.shares.size).toBe(1);
    expect((await create(service, shareId, { owner: ownerB })).status).toBe(409);
  });

  test("installation and IP limits apply before inserting", async () => {
    const first = fixture();
    for (let index = 0; index < 10; index += 1) expect((await create(first.service, id(100 + index), { ip: index.toString(16).padStart(64, "0") })).status).toBe(201);
    expect((await create(first.service, id(111), { ip: "f".repeat(64) })).status).toBe(429);
    expect(first.metadata.shares.size).toBe(10);

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
    expect(metadata.shares.size).toBe(100);
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
    first.metadata.getShare(byteLimitedId).total_bytes = LIMITS.shareBytes;
    const byteResponse = await upload(first.service, byteLimitedId, "extra.json", "{}");
    expect(byteResponse.status).toBe(413);
    expect((await byteResponse.json()).error).toBe("share_bytes");
    expect(first.bucket.objects.size).toBe(0);

    const second = fixture();
    const fileLimitedId = id(53);
    await create(second.service, fileLimitedId);
    second.metadata.getShare(fileLimitedId).file_count = LIMITS.shareFiles;
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

  test("delete waits behind an in-flight upload and removes its object", async () => {
    const { service, bucket } = fixture();
    const shareId = id(9);
    await create(service, shareId);
    let release;
    let started;
    const putStarted = new Promise((resolve) => { started = resolve; });
    bucket.onPutStarted = started;
    bucket.putGate = new Promise((resolve) => { release = resolve; });
    const putting = upload(service, shareId, "shots/001.jpg", "image");
    await putStarted;
    const deleting = service.fetch(internalRequest(`/api/shares/${shareId}`, { method: "DELETE" }));
    release();
    expect((await putting).status).toBe(201);
    expect((await deleting).status).toBe(204);
    expect(bucket.objects.has(`${shareId}/shots/001.jpg`)).toBe(false);
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
