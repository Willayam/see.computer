// Isolated benchmark host. Public reads of synthetic fixtures, owner-authorized writes.
// Production recording accounts, quotas, deletion, and billing are outside this experiment.
const maximumBytes = 8 * 1024 * 1024;
const types = {
  mp4: "video/mp4", m4s: "video/iso.segment", m3u8: "application/vnd.apple.mpegurl",
  png: "image/png", jpg: "image/jpeg", json: "application/json", md: "text/plain; charset=utf-8",
  html: "text/html; charset=utf-8", csv: "text/csv; charset=utf-8",
};

function response(body, status = 200, headers = {}) {
  return new Response(body, { status, headers: {
    "X-Content-Type-Options": "nosniff", "Referrer-Policy": "no-referrer",
    "Content-Security-Policy": "default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; frame-ancestors 'none'",
    ...headers,
  } });
}

export default {
  async fetch(request, env) {
    const path = new URL(request.url).pathname;
    if (path === "/") return response("see.computer media benchmark. Open a published experiment link.\n");
    const match = /^\/d\/([a-f0-9]{32})\/([a-zA-Z0-9_-]+(?:\/[a-zA-Z0-9_-]+)*\.([a-z0-9]+))$/.exec(path);
    if (!match || !types[match[3]]) return response("Not found", 404);
    const key = `${match[1]}/${match[2]}`;
    if (request.method === "PUT") {
      if (!env.BENCH_UPLOAD_TOKEN || request.headers.get("Authorization") !== `Bearer ${env.BENCH_UPLOAD_TOKEN}`) {
        return response("Unauthorized", 401);
      }
      const declared = Number(request.headers.get("Content-Length"));
      if (!Number.isSafeInteger(declared) || declared <= 0 || declared > maximumBytes || !request.body) {
        return response("A nonempty object up to 8 MiB is required", 413);
      }
      const reader = request.body.getReader();
      const chunks = [];
      let bytes = 0;
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        bytes += value.byteLength;
        if (bytes > maximumBytes || bytes > declared) {
          await reader.cancel();
          return response("Object too large", 413);
        }
        chunks.push(value);
      }
      if (bytes !== declared) return response("Length mismatch", 400);
      await env.MEDIA.put(key, await new Blob(chunks).arrayBuffer(), {
        httpMetadata: { contentType: types[match[3]], cacheControl: "no-store" },
      });
      return response(null, 201);
    }
    if (request.method !== "GET" && request.method !== "HEAD") return response("Method not allowed", 405, { Allow: "GET, HEAD, PUT" });
    const metadata = await env.MEDIA.head(key);
    if (!metadata) return response("Not found", 404);
    const headers = {
      "Content-Type": types[match[3]], "Accept-Ranges": "bytes", "Cache-Control": "no-store",
      ETag: metadata.httpEtag, "Content-Length": String(metadata.size),
    };
    if (request.method === "HEAD") return response(null, 200, headers);
    if (request.headers.get("If-None-Match") === metadata.httpEtag) {
      delete headers["Content-Length"];
      return response(null, 304, headers);
    }
    let range;
    const requested = request.headers.get("Range");
    if (requested) {
      const parts = /^bytes=(\d*)-(\d*)$/.exec(requested);
      if (!parts || (!parts[1] && !parts[2])) return response(null, 416, { "Content-Range": `bytes */${metadata.size}` });
      const offset = parts[1] ? Number(parts[1]) : Math.max(0, metadata.size - Number(parts[2]));
      const end = parts[1] && parts[2] ? Math.min(Number(parts[2]), metadata.size - 1) : metadata.size - 1;
      if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(end) || offset > end || offset >= metadata.size) {
        return response(null, 416, { "Content-Range": `bytes */${metadata.size}` });
      }
      range = { offset, length: end - offset + 1 };
      headers["Content-Length"] = String(range.length);
      headers["Content-Range"] = `bytes ${offset}-${end}/${metadata.size}`;
    }
    const object = await env.MEDIA.get(key, range ? { range } : undefined);
    if (!object) return response("Not found", 404);
    return response(object.body, range ? 206 : 200, headers);
  },
};
