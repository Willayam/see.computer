import { rootHtml, viewerHtml } from "./viewer.mjs";

export const LIMITS = Object.freeze({
  objectBytes: 8 * 1024 * 1024,
  shareBytes: 256 * 1024 * 1024,
  shareFiles: 1500,
  installationMonth: 10,
  ipDay: 20,
  globalDay: 100,
  retentionMs: 30 * 24 * 60 * 60 * 1000,
});

export const CONTENT_TYPES = Object.freeze({
  png: "image/png",
  jpg: "image/jpeg",
  mp4: "video/mp4",
  mov: "video/quicktime",
  m4s: "video/iso.segment",
  m3u8: "application/vnd.apple.mpegurl",
  json: "application/json; charset=utf-8",
  md: "text/markdown; charset=utf-8",
});

const shareIdPattern = "([a-f0-9]{32})";
const relativePattern = /^[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*(?:\/[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*)*\.([a-z0-9]+)$/;
const pageCsp = (nonce) => `default-src 'none'; script-src 'nonce-${nonce}'; style-src 'unsafe-inline'; img-src 'self'; media-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`;
const staticPageCsp = "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";
const assetCsp = "default-src 'none'; img-src 'self'; media-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

function baseHeaders(csp = assetCsp) {
  return {
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "no-referrer",
    "Content-Security-Policy": csp,
  };
}

export function response(body, status = 200, headers = {}, csp = assetCsp) {
  return new Response(body, { status, headers: { ...baseHeaders(csp), ...headers } });
}

function apiError(status, code, message, headers = {}) {
  return response(JSON.stringify({ error: code, message }), status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
    ...headers,
  });
}

function notFound() {
  return response("Not found\n", 404, { "Content-Type": "text/plain; charset=utf-8", "Cache-Control": "no-store" });
}

export function parseRoute(pathname) {
  let match = new RegExp(`^/api/shares/${shareIdPattern}$`).exec(pathname);
  if (match) return { kind: "share", id: match[1] };
  match = new RegExp(`^/api/shares/${shareIdPattern}/complete$`).exec(pathname);
  if (match) return { kind: "complete", id: match[1] };
  match = new RegExp(`^/api/shares/${shareIdPattern}/(.+)$`).exec(pathname);
  if (match) return { kind: "upload", id: match[1], rawPath: match[2] };
  match = new RegExp(`^/s/${shareIdPattern}/$`).exec(pathname);
  if (match) return { kind: "page", id: match[1] };
  match = new RegExp(`^/s/${shareIdPattern}/status\.json$`).exec(pathname);
  if (match) return { kind: "status", id: match[1] };
  match = new RegExp(`^/s/${shareIdPattern}/(.+)$`).exec(pathname);
  if (match) return { kind: "asset", id: match[1], rawPath: match[2] };
  return null;
}

export function validateRelativePath(rawPath) {
  if (!rawPath || rawPath.length > 128 || rawPath.includes("%") || rawPath.includes("\\") || /[\u0000-\u001f\u007f]/.test(rawPath)) return null;
  const match = relativePattern.exec(rawPath);
  const extension = match && (match[1] || match[2]);
  if (!extension || !Object.hasOwn(CONTENT_TYPES, extension)) return null;
  return { path: rawPath, extension, contentType: CONTENT_TYPES[extension] };
}

export function parseByteRange(value, size) {
  if (!value) return null;
  const match = /^bytes=(\d*)-(\d*)$/.exec(value);
  if (!match || (!match[1] && !match[2]) || !Number.isSafeInteger(size) || size <= 0) return false;
  let start;
  let end;
  if (!match[1]) {
    const suffix = Number(match[2]);
    if (!Number.isSafeInteger(suffix) || suffix <= 0) return false;
    start = Math.max(0, size - suffix);
    end = size - 1;
  } else {
    start = Number(match[1]);
    end = match[2] ? Math.min(Number(match[2]), size - 1) : size - 1;
  }
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start > end || start >= size) return false;
  return { offset: start, length: end - start + 1, end };
}

export function rewriteMarkdownPaths(markdown, shareUrl) {
  return markdown.replace(/(!?\[[^\]\n]*\]\()([^\s)]+)(\))/g, (whole, before, target, after) => {
    const valid = validateRelativePath(target);
    if (!valid) return whole;
    return `${before}${new URL(valid.path, shareUrl).href}${after}`;
  });
}

function etagMatches(header, etag) {
  if (!header) return false;
  return header.split(",").some((candidate) => {
    const value = candidate.trim();
    return value === "*" || value === etag || value.replace(/^W\//, "") === etag.replace(/^W\//, "");
  });
}

function bytesToHex(bytes) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

export async function sha256Hex(value) {
  const bytes = typeof value === "string" ? new TextEncoder().encode(value) : value;
  return bytesToHex(new Uint8Array(await crypto.subtle.digest("SHA-256", bytes)));
}

function randomNonce() {
  const bytes = crypto.getRandomValues(new Uint8Array(18));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export async function readBoundedBody(request, maximum = LIMITS.objectBytes) {
  if (!request.body) return { ok: false, status: 400, message: "A nonempty request body is required" };
  const declaredHeader = request.headers.get("Content-Length");
  const declared = declaredHeader === null ? null : Number(declaredHeader);
  if (declared !== null && (!Number.isSafeInteger(declared) || declared <= 0)) return { ok: false, status: 400, message: "Content-Length must describe a nonempty body" };
  if (declared !== null && declared > maximum) return { ok: false, status: 413, message: "Object exceeds the 8 MiB limit" };
  const reader = request.body.getReader();
  const bytes = new Uint8Array(declared ?? maximum);
  let length = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > maximum || (declared !== null && length > declared)) {
        await reader.cancel();
        return { ok: false, status: 413, message: "Object exceeds its allowed length" };
      }
      bytes.set(value, length - value.byteLength);
    }
  } catch {
    return { ok: false, status: 400, message: "Request body could not be read" };
  }
  if (length === 0) return { ok: false, status: 400, message: "A nonempty request body is required" };
  if (declared !== null && length !== declared) return { ok: false, status: 400, message: "Content-Length does not match the request body" };
  return { ok: true, bytes: bytes.subarray(0, length) };
}

async function readJson(request) {
  const body = await readBoundedBody(request, 4096);
  if (!body.ok) return body;
  try {
    return { ok: true, value: JSON.parse(new TextDecoder().decode(body.bytes)) };
  } catch {
    return { ok: false, status: 400, message: "Request body must be valid JSON" };
  }
}

async function requireEmptyBody(request) {
  if (!request.body) return true;
  const reader = request.body.getReader();
  const { value, done } = await reader.read();
  if (!done || (value && value.byteLength > 0)) {
    await reader.cancel();
    return false;
  }
  return true;
}

function dayAndMonthStarts(now) {
  const date = new Date(now);
  return {
    dayStart: Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate()),
    monthStart: Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), 1),
  };
}

function publicState(share, now) {
  return share && share.state !== "deleted" && share.expires_at > now;
}

export class SqliteMetadata {
  constructor(storage) {
    this.storage = storage;
    this.sql = storage.sql;
    this.sql.exec(`
      CREATE TABLE IF NOT EXISTS shares (
        id TEXT PRIMARY KEY,
        owner_hash TEXT NOT NULL,
        ip_hash TEXT NOT NULL,
        url TEXT NOT NULL,
        expires_at INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('open', 'ready', 'deleted')),
        total_bytes INTEGER NOT NULL DEFAULT 0,
        file_count INTEGER NOT NULL DEFAULT 0
      );
      CREATE INDEX IF NOT EXISTS shares_owner_created ON shares(owner_hash, created_at);
      CREATE INDEX IF NOT EXISTS shares_ip_created ON shares(ip_hash, created_at);
      CREATE INDEX IF NOT EXISTS shares_created ON shares(created_at);
      CREATE TABLE IF NOT EXISTS files (
        share_id TEXT NOT NULL,
        path TEXT NOT NULL,
        committed_size INTEGER,
        committed_sha TEXT,
        pending_size INTEGER,
        pending_sha TEXT,
        PRIMARY KEY (share_id, path),
        FOREIGN KEY (share_id) REFERENCES shares(id)
      );
      CREATE INDEX IF NOT EXISTS files_pending ON files(share_id, pending_sha);
    `);
  }

  one(query, ...bindings) {
    return this.sql.exec(query, ...bindings).toArray()[0] ?? null;
  }

  getShare(id) {
    return this.one("SELECT * FROM shares WHERE id = ?", id);
  }

  getFile(id, path) {
    return this.one("SELECT * FROM files WHERE share_id = ? AND path = ?", id, path);
  }

  pendingFiles(id) {
    return this.sql.exec("SELECT * FROM files WHERE share_id = ? AND pending_sha IS NOT NULL", id).toArray();
  }

  createShare(input) {
    return this.storage.transactionSync(() => {
      const existing = this.getShare(input.id);
      if (existing) return { kind: "existing", share: existing };
      const installationCount = this.one("SELECT COUNT(*) AS count FROM shares WHERE owner_hash = ? AND created_at >= ?", input.ownerHash, input.monthStart).count;
      if (installationCount >= LIMITS.installationMonth) return { kind: "quota", quota: "installation_month" };
      const globalCount = this.one("SELECT COUNT(*) AS count FROM shares WHERE created_at >= ?", input.dayStart).count;
      if (globalCount >= LIMITS.globalDay) return { kind: "quota", quota: "global_day" };
      const ipCount = this.one("SELECT COUNT(*) AS count FROM shares WHERE ip_hash = ? AND created_at >= ?", input.ipHash, input.dayStart).count;
      if (ipCount >= LIMITS.ipDay) return { kind: "quota", quota: "ip_day" };
      this.sql.exec(
        "INSERT INTO shares (id, owner_hash, ip_hash, url, expires_at, created_at, state) VALUES (?, ?, ?, ?, ?, ?, 'open')",
        input.id, input.ownerHash, input.ipHash, input.url, input.expiresAt, input.now,
      );
      return { kind: "created", share: this.getShare(input.id) };
    });
  }

  reserveUpload(id, path, size, sha, now) {
    return this.storage.transactionSync(() => {
      const share = this.getShare(id);
      if (!share) return { kind: "missing" };
      if (share.state !== "open") return { kind: "state", state: share.state };
      if (share.expires_at <= now) return { kind: "expired" };
      const file = this.getFile(id, path);
      if (file?.pending_sha) return { kind: "pending", file };
      const oldSize = file?.committed_size ?? 0;
      const delta = size - oldSize;
      const nextCount = share.file_count + (file?.committed_size === null || !file ? 1 : 0);
      if (share.total_bytes + delta > LIMITS.shareBytes) return { kind: "quota", quota: "share_bytes" };
      if (nextCount > LIMITS.shareFiles) return { kind: "quota", quota: "share_files" };
      this.sql.exec("UPDATE shares SET total_bytes = total_bytes + ?, file_count = ? WHERE id = ?", delta, nextCount, id);
      if (file) {
        this.sql.exec("UPDATE files SET pending_size = ?, pending_sha = ? WHERE share_id = ? AND path = ?", size, sha, id, path);
      } else {
        this.sql.exec("INSERT INTO files (share_id, path, pending_size, pending_sha) VALUES (?, ?, ?, ?)", id, path, size, sha);
      }
      return { kind: "reserved" };
    });
  }

  finalizeUpload(id, path, sha) {
    return this.storage.transactionSync(() => {
      const file = this.getFile(id, path);
      if (!file || file.pending_sha !== sha) return false;
      this.sql.exec(
        "UPDATE files SET committed_size = pending_size, committed_sha = pending_sha, pending_size = NULL, pending_sha = NULL WHERE share_id = ? AND path = ?",
        id, path,
      );
      return true;
    });
  }

  rollbackUpload(id, path, sha) {
    return this.storage.transactionSync(() => {
      const file = this.getFile(id, path);
      if (!file || file.pending_sha !== sha) return false;
      const committedSize = file.committed_size ?? 0;
      const delta = file.pending_size - committedSize;
      const countDelta = file.committed_size === null ? 1 : 0;
      this.sql.exec("UPDATE shares SET total_bytes = total_bytes - ?, file_count = file_count - ? WHERE id = ?", delta, countDelta, id);
      if (file.committed_size === null) this.sql.exec("DELETE FROM files WHERE share_id = ? AND path = ?", id, path);
      else this.sql.exec("UPDATE files SET pending_size = NULL, pending_sha = NULL WHERE share_id = ? AND path = ?", id, path);
      return true;
    });
  }

  markReady(id, ownerHash, now) {
    return this.storage.transactionSync(() => {
      const share = this.getShare(id);
      if (!share) return { kind: "missing" };
      if (share.owner_hash !== ownerHash) return { kind: "owner" };
      if (share.state === "deleted") return { kind: "state", state: "deleted" };
      if (share.expires_at <= now) return { kind: "expired" };
      if (share.state === "ready") return { kind: "ready", share };
      const transcript = this.getFile(id, "transcript.json");
      const take = this.getFile(id, "take.md");
      if (!transcript?.committed_sha || !take?.committed_sha) return { kind: "incomplete" };
      this.sql.exec("UPDATE shares SET state = 'ready' WHERE id = ?", id);
      return { kind: "ready", share: this.getShare(id) };
    });
  }

  markDeleted(id, ownerHash) {
    return this.storage.transactionSync(() => {
      const share = this.getShare(id);
      if (!share) return { kind: "missing" };
      if (share.owner_hash !== ownerHash) return { kind: "owner" };
      if (share.state !== "deleted") this.sql.exec("UPDATE shares SET state = 'deleted' WHERE id = ?", id);
      return { kind: "deleted" };
    });
  }
}

export class ShareCoordinatorService {
  constructor(metadata, bucket, options = {}) {
    this.metadata = metadata;
    this.bucket = bucket;
    this.now = options.now ?? (() => Date.now());
    this.publicOrigin = options.publicOrigin ?? "https://see-computer-shares.cocco.workers.dev";
    this.mutationTail = Promise.resolve();
  }

  mutate(operation) {
    const result = this.mutationTail.then(operation, operation);
    this.mutationTail = result.catch(() => {});
    return result;
  }

  async fetch(request) {
    const url = new URL(request.url);
    const route = parseRoute(url.pathname);
    if (!route) return notFound();
    if (route.kind === "share" && request.method === "POST") return this.create(request, route.id);
    if (route.kind === "share" && request.method === "DELETE") return this.remove(request, route.id);
    if (route.kind === "complete" && request.method === "POST") return this.complete(request, route.id);
    if (route.kind === "upload" && request.method === "PUT") return this.upload(request, route.id, route.rawPath);
    if (route.kind === "page" && (request.method === "GET" || request.method === "HEAD")) return this.page(request, route.id);
    if (route.kind === "status" && (request.method === "GET" || request.method === "HEAD")) return this.status(request, route.id);
    if (route.kind === "asset" && (request.method === "GET" || request.method === "HEAD")) return this.asset(request, route.id, route.rawPath);
    const allow = route.kind === "share" ? "POST, DELETE" : route.kind === "complete" ? "POST" : route.kind === "upload" ? "PUT" : "GET, HEAD";
    return apiError(405, "method_not_allowed", "Method not allowed", { Allow: allow });
  }

  owner(request) {
    const value = request.headers.get("X-Share-Owner");
    return /^[a-f0-9]{64}$/.test(value ?? "") ? value : null;
  }

  async create(request, id) {
    const ownerHash = this.owner(request);
    const ipHash = request.headers.get("X-Share-IP");
    if (!ownerHash || !/^[a-f0-9]{64}$/.test(ipHash ?? "")) return apiError(401, "unauthorized", "A valid installation secret is required");
    const empty = !request.body || request.headers.get("Content-Length") === "0";
    const parsed = empty ? { ok: true, value: {} } : await readJson(request);
    if (!parsed.ok) return apiError(parsed.status, "invalid_request", parsed.message);
    const now = this.now();
    const expectedUrl = `${this.publicOrigin}/s/${id}/`;
    const expiresAt = empty ? now + LIMITS.retentionMs : typeof parsed.value?.expiresAt === "string" ? Date.parse(parsed.value.expiresAt) : NaN;
    if ((!empty && parsed.value?.url !== expectedUrl) || !Number.isFinite(expiresAt) || expiresAt <= now || expiresAt > now + LIMITS.retentionMs) {
      return apiError(400, "invalid_share", "url must match this share and expiresAt must be within 30 days");
    }
    const starts = dayAndMonthStarts(now);
    return this.mutate(async () => {
      const result = this.metadata.createShare({ id, ownerHash, ipHash, url: expectedUrl, expiresAt, now, ...starts });
      if (result.kind === "existing") {
        const share = result.share;
        if (share.owner_hash !== ownerHash) return apiError(409, "share_id_unavailable", "Share ID is already in use");
        if (share.state === "deleted") return apiError(409, "share_deleted", "A canceled share cannot be recreated");
        if (share.url !== expectedUrl || (!empty && share.expires_at !== expiresAt)) return apiError(409, "share_conflict", "Share already exists with different values");
        return this.shareJson(share, 200);
      }
      if (result.kind === "quota") return apiError(429, result.quota, "Share creation limit reached", { "Retry-After": "3600" });
      return this.shareJson(result.share, 201);
    });
  }

  shareJson(share, status) {
    return response(JSON.stringify({ url: share.url, expiresAt: new Date(share.expires_at).toISOString(), state: share.state === "open" ? "uploading" : share.state }), status, {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
    });
  }

  async reconcilePending(id) {
    for (const file of this.metadata.pendingFiles(id)) {
      const object = await this.bucket.head(`${id}/${file.path}`);
      if (object && object.size === file.pending_size && object.customMetadata?.sha256 === file.pending_sha) {
        this.metadata.finalizeUpload(id, file.path, file.pending_sha);
      } else {
        this.metadata.rollbackUpload(id, file.path, file.pending_sha);
      }
    }
  }

  async upload(request, id, rawPath) {
    const ownerHash = this.owner(request);
    if (!ownerHash) return apiError(401, "unauthorized", "A valid installation secret is required");
    const valid = validateRelativePath(rawPath);
    if (!valid) return apiError(400, "invalid_path", "Upload path or file type is not allowed");
    const preliminary = this.metadata.getShare(id);
    const preliminaryNow = this.now();
    if (!preliminary) return apiError(404, "not_found", "Share not found");
    if (preliminary.owner_hash !== ownerHash) return apiError(403, "forbidden", "This installation does not own the share");
    if (preliminary.state === "deleted") return apiError(409, "share_deleted", "Canceled shares cannot accept uploads");
    if (preliminary.state !== "open") return apiError(409, "share_ready", "Completed shares cannot accept uploads");
    if (preliminary.expires_at <= preliminaryNow) return apiError(410, "share_expired", "Share has expired");
    const body = await readBoundedBody(request);
    if (!body.ok) return apiError(body.status, "invalid_body", body.message);
    if (request.signal.aborted) return apiError(499, "request_canceled", "Request was canceled before upload");
    const sha = await sha256Hex(body.bytes);
    return this.mutate(async () => {
      let share = this.metadata.getShare(id);
      const now = this.now();
      if (!share) return apiError(404, "not_found", "Share not found");
      if (share.owner_hash !== ownerHash) return apiError(403, "forbidden", "This installation does not own the share");
      if (share.state === "deleted") return apiError(409, "share_deleted", "Canceled shares cannot accept uploads");
      if (share.state !== "open") return apiError(409, "share_ready", "Completed shares cannot accept uploads");
      if (share.expires_at <= now) return apiError(410, "share_expired", "Share has expired");
      try {
        await this.reconcilePending(id);
      } catch {
        return apiError(502, "storage_error", "Object storage could not reconcile an earlier upload");
      }
      share = this.metadata.getShare(id);
      if (!share || share.state !== "open" || share.expires_at <= now) return apiError(409, "share_changed", "Share state changed before upload");
      const existing = this.metadata.getFile(id, valid.path);
      const key = `${id}/${valid.path}`;
      if (existing?.committed_sha === sha && existing.committed_size === body.bytes.byteLength) {
        try {
          const object = await this.bucket.head(key);
          if (!object || object.size !== body.bytes.byteLength || object.customMetadata?.sha256 !== sha) {
            await this.bucket.put(key, body.bytes, {
              httpMetadata: { contentType: valid.contentType, cacheControl: "no-store" },
              customMetadata: { sha256: sha },
            });
          }
        } catch {
          return apiError(502, "storage_error", "Object storage rejected the upload");
        }
        return response(null, 201, { "Cache-Control": "no-store" });
      }
      const reservation = this.metadata.reserveUpload(id, valid.path, body.bytes.byteLength, sha, now);
      if (reservation.kind === "quota") return apiError(413, reservation.quota, "Share storage limit reached");
      if (reservation.kind !== "reserved") return apiError(409, "share_changed", "Share state changed before upload");
      try {
        await this.bucket.put(key, body.bytes, {
          httpMetadata: { contentType: valid.contentType, cacheControl: "no-store" },
          customMetadata: { sha256: sha },
        });
      } catch {
        this.metadata.rollbackUpload(id, valid.path, sha);
        return apiError(502, "storage_error", "Object storage rejected the upload");
      }
      if (!this.metadata.finalizeUpload(id, valid.path, sha)) return apiError(500, "metadata_error", "Upload metadata could not be committed");
      return response(null, 201, { "Cache-Control": "no-store" });
    });
  }

  async complete(request, id) {
    const ownerHash = this.owner(request);
    if (!ownerHash) return apiError(401, "unauthorized", "A valid installation secret is required");
    if (!(await requireEmptyBody(request))) return apiError(400, "invalid_body", "Completion request body must be empty");
    return this.mutate(async () => {
      let share = this.metadata.getShare(id);
      const now = this.now();
      if (!share) return apiError(404, "not_found", "Share not found");
      if (share.owner_hash !== ownerHash) return apiError(403, "forbidden", "This installation does not own the share");
      if (share.state === "deleted") return apiError(409, "share_deleted", "Canceled shares cannot be completed");
      if (share.expires_at <= now) return apiError(410, "share_expired", "Share has expired");
      try {
        await this.reconcilePending(id);
      } catch {
        return apiError(502, "storage_error", "Object storage could not reconcile an earlier upload");
      }
      share = this.metadata.getShare(id);
      if (!share || share.state === "deleted" || share.expires_at <= now) return apiError(409, "share_changed", "Share state changed before completion");
      const transcriptFile = this.metadata.getFile(id, "transcript.json");
      const takeFile = this.metadata.getFile(id, "take.md");
      if (!transcriptFile?.committed_sha || !takeFile?.committed_sha) {
        return apiError(409, "missing_required_files", "transcript.json and take.md must be uploaded first");
      }
      let transcript;
      let take;
      try {
        [transcript, take] = await Promise.all([
          this.bucket.head(`${id}/transcript.json`),
          this.bucket.head(`${id}/take.md`),
        ]);
      } catch {
        return apiError(502, "storage_error", "Object storage could not verify required files");
      }
      if (
        !transcript || !take ||
        transcript.size !== transcriptFile.committed_size || transcript.customMetadata?.sha256 !== transcriptFile.committed_sha ||
        take.size !== takeFile.committed_size || take.customMetadata?.sha256 !== takeFile.committed_sha
      ) return apiError(409, "missing_required_files", "transcript.json and take.md must exist in object storage");
      const result = this.metadata.markReady(id, ownerHash, now);
      if (result.kind === "missing") return apiError(404, "not_found", "Share not found");
      if (result.kind === "owner") return apiError(403, "forbidden", "This installation does not own the share");
      if (result.kind === "expired") return apiError(410, "share_expired", "Share has expired");
      if (result.kind === "state") return apiError(409, "share_deleted", "Canceled shares cannot be completed");
      if (result.kind === "incomplete") return apiError(409, "missing_required_files", "transcript.json and take.md must be uploaded first");
      return this.shareJson(result.share, 200);
    });
  }

  async remove(request, id) {
    const ownerHash = this.owner(request);
    if (!ownerHash) return apiError(401, "unauthorized", "A valid installation secret is required");
    return this.mutate(async () => {
      // A timed-out create can still be in flight. Reserve a tombstone even when
      // DELETE reaches the coordinator first, using the same creation limits.
      if (!this.metadata.getShare(id)) {
        const now = this.now();
        const ipHash = request.headers.get("X-Share-IP");
        if (!/^[a-f0-9]{64}$/.test(ipHash ?? "")) return apiError(401, "unauthorized", "Missing client identity");
        const reserved = this.metadata.createShare({ id, ownerHash, ipHash,
          url: `${this.publicOrigin}/s/${id}/`, expiresAt: now + LIMITS.retentionMs,
          now, ...dayAndMonthStarts(now) });
        if (reserved.kind === "quota") return notFound();
      }
      const result = this.metadata.markDeleted(id, ownerHash);
      if (result.kind === "missing") return apiError(404, "not_found", "Share not found");
      if (result.kind === "owner") return apiError(403, "forbidden", "This installation does not own the share");
      try {
        const keys = [];
        let cursor;
        do {
          const listed = await this.bucket.list({ prefix: `${id}/`, cursor });
          for (const object of listed.objects) keys.push(object.key);
          cursor = listed.truncated ? listed.cursor : undefined;
        } while (cursor);
        for (let index = 0; index < keys.length; index += 1000) await this.bucket.delete(keys.slice(index, index + 1000));
      } catch {
        return apiError(502, "storage_error", "Share is canceled, but object cleanup must be retried");
      }
      return response(null, 204, { "Cache-Control": "no-store" });
    });
  }

  page(request, id) {
    const share = this.metadata.getShare(id);
    if (!publicState(share, this.now())) return notFound();
    const nonce = randomNonce();
    const body = viewerHtml(nonce);
    const headers = { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store", "Content-Length": String(new TextEncoder().encode(body).byteLength) };
    return response(request.method === "HEAD" ? null : body, 200, headers, pageCsp(nonce));
  }

  status(request, id) {
    const share = this.metadata.getShare(id);
    if (!publicState(share, this.now())) return notFound();
    const body = JSON.stringify({ state: share.state === "ready" ? "ready" : "uploading", expiresAt: new Date(share.expires_at).toISOString() });
    return response(request.method === "HEAD" ? null : body, 200, {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
      "Content-Length": String(new TextEncoder().encode(body).byteLength),
    });
  }

  async asset(request, id, rawPath) {
    const valid = validateRelativePath(rawPath);
    if (!valid) return notFound();
    const share = this.metadata.getShare(id);
    if (!publicState(share, this.now())) return notFound();
    const file = this.metadata.getFile(id, valid.path);
    if (!file?.committed_sha) return notFound();
    const key = `${id}/${valid.path}`;
    const object = await this.bucket.head(key);
    if (!object || object.size !== file.committed_size || object.customMetadata?.sha256 !== file.committed_sha) return notFound();
    const url = new URL(request.url);
    if (valid.path === "take.md" && url.searchParams.get("absolute") === "1") return this.absoluteMarkdown(request, id);
    const headers = {
      "Content-Type": valid.contentType,
      "Accept-Ranges": "bytes",
      "Cache-Control": "no-store",
      ETag: object.httpEtag,
      "Content-Length": String(object.size),
    };
    if (etagMatches(request.headers.get("If-None-Match"), object.httpEtag)) {
      delete headers["Content-Length"];
      return response(null, 304, headers);
    }
    const range = parseByteRange(request.headers.get("Range"), object.size);
    if (range === false) return response(null, 416, { "Content-Range": `bytes */${object.size}`, "Accept-Ranges": "bytes", "Cache-Control": "no-store" });
    if (range) {
      headers["Content-Length"] = String(range.length);
      headers["Content-Range"] = `bytes ${range.offset}-${range.end}/${object.size}`;
    }
    if (request.method === "HEAD") return response(null, range ? 206 : 200, headers);
    const stored = await this.bucket.get(key, range ? { range: { offset: range.offset, length: range.length } } : undefined);
    if (!stored) return notFound();
    return response(stored.body, range ? 206 : 200, headers);
  }

  async absoluteMarkdown(request, id) {
    const stored = await this.bucket.get(`${id}/take.md`);
    if (!stored) return notFound();
    const source = await new Response(stored.body).text();
    const body = rewriteMarkdownPaths(source, `${this.publicOrigin}/s/${id}/`);
    const bytes = new TextEncoder().encode(body);
    const etag = `"sha256-${await sha256Hex(bytes)}"`;
    const headers = {
      "Content-Type": CONTENT_TYPES.md,
      "Accept-Ranges": "bytes",
      "Cache-Control": "no-store",
      ETag: etag,
      "Content-Length": String(bytes.byteLength),
    };
    if (etagMatches(request.headers.get("If-None-Match"), etag)) {
      delete headers["Content-Length"];
      return response(null, 304, headers);
    }
    const range = parseByteRange(request.headers.get("Range"), bytes.byteLength);
    if (range === false) return response(null, 416, { "Content-Range": `bytes */${bytes.byteLength}`, "Accept-Ranges": "bytes", "Cache-Control": "no-store" });
    if (range) {
      headers["Content-Length"] = String(range.length);
      headers["Content-Range"] = `bytes ${range.offset}-${range.end}/${bytes.byteLength}`;
    }
    if (request.method === "HEAD") return response(null, range ? 206 : 200, headers);
    return response(range ? bytes.slice(range.offset, range.offset + range.length) : bytes, range ? 206 : 200, headers);
  }
}

export class Coordinator {
  constructor(ctx, env) {
    this.service = new ShareCoordinatorService(new SqliteMetadata(ctx.storage), env.SHARES, {
      publicOrigin: env.PUBLIC_ORIGIN,
    });
  }

  fetch(request) {
    return this.service.fetch(request);
  }
}

async function authorizationHashes(request) {
  const authorization = request.headers.get("Authorization") ?? "";
  const match = /^Bearer ([a-f0-9]{64})$/.exec(authorization);
  if (!match) return null;
  const ip = request.headers.get("CF-Connecting-IP");
  return {
    ownerHash: await sha256Hex(match[1]),
    ipHash: ip ? await sha256Hex(ip) : null,
  };
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/hls-1.7.2.min.js" || url.pathname === "/hls-LICENSE") {
      return env.ASSETS.fetch(request);
    }
    if (url.pathname === "/" && (request.method === "GET" || request.method === "HEAD")) {
      const body = rootHtml();
      return response(request.method === "HEAD" ? null : body, 200, {
        "Content-Type": "text/html; charset=utf-8",
        "Cache-Control": "no-store",
        "Content-Length": String(new TextEncoder().encode(body).byteLength),
      }, staticPageCsp);
    }
    const route = parseRoute(url.pathname);
    if (!route) return notFound();
    const internal = new Request(`https://coordinator.internal${url.pathname}${url.search}`, request);
    internal.headers.delete("Authorization");
    internal.headers.delete("X-Share-Owner");
    internal.headers.delete("X-Share-IP");
    if (route.kind === "share" || route.kind === "complete" || route.kind === "upload") {
      const hashes = await authorizationHashes(request);
      if (!hashes?.ownerHash) return apiError(401, "unauthorized", "Authorization must be Bearer followed by 64 lowercase hexadecimal characters", { "WWW-Authenticate": "Bearer" });
      internal.headers.set("X-Share-Owner", hashes.ownerHash);
      if (hashes.ipHash) internal.headers.set("X-Share-IP", hashes.ipHash);
    }
    const id = env.COORDINATOR.idFromName("global-v1");
    return env.COORDINATOR.get(id).fetch(internal);
  },
};
