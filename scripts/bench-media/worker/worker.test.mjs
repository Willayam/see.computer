import { test, expect } from "bun:test";
import worker from "./worker.mjs";

function fixture() {
  const objects = new Map();
  return { BENCH_UPLOAD_TOKEN: "test-token", MEDIA: {
    async put(key, bytes) { objects.set(key, new Uint8Array(bytes)); },
    async head(key) { return objects.has(key) ? { size: objects.get(key).length, httpEtag: '"test"' } : null; },
    async get(key, options) {
      const bytes = objects.get(key);
      if (!bytes) return null;
      const range = options?.range;
      return { body: range ? bytes.slice(range.offset, range.offset + range.length) : bytes };
    },
  } };
}
const url = "https://example.test/d/0123456789abcdef0123456789abcdef/video.mp4";
const put = (body, authorization = "Bearer test-token", length = body.length) => new Request(url, {
  method: "PUT", headers: { Authorization: authorization, "Content-Length": String(length) }, body,
});

test("owner upload, public read, HEAD and video byte ranges", async () => {
  const env = fixture();
  expect((await worker.fetch(put("abcdefghij"), env)).status).toBe(201);
  expect(await (await worker.fetch(new Request(url), env)).text()).toBe("abcdefghij");
  const head = await worker.fetch(new Request(url, { method: "HEAD" }), env);
  expect(head.headers.get("Content-Length")).toBe("10");
  expect(await head.text()).toBe("");
  for (const [range, expected, contentRange] of [["bytes=2-5", "cdef", "bytes 2-5/10"], ["bytes=-3", "hij", "bytes 7-9/10"], ["bytes=8-", "ij", "bytes 8-9/10"]]) {
    const result = await worker.fetch(new Request(url, { headers: { Range: range } }), env);
    expect(result.status).toBe(206);
    expect(result.headers.get("Content-Range")).toBe(contentRange);
    expect(await result.text()).toBe(expected);
  }
  expect((await worker.fetch(new Request(url, { headers: { Range: "bytes=20-30" } }), env)).status).toBe(416);
});

test("rejects unauthorized writes, oversized and misleading lengths", async () => {
  const env = fixture();
  expect((await worker.fetch(put("data", "Bearer wrong"), env)).status).toBe(401);
  expect((await worker.fetch(put("data", "Bearer test-token", 9 * 1024 * 1024), env)).status).toBe(413);
  expect((await worker.fetch(put("longer", "Bearer test-token", 2), env)).status).toBe(413);
  expect((await worker.fetch(put("short", "Bearer test-token", 10), env)).status).toBe(400);
  expect((await worker.fetch(new Request(url), env)).status).toBe(404);
});

test("rejects unknown paths and methods", async () => {
  const env = fixture();
  expect((await worker.fetch(new Request(url + ".exe"), env)).status).toBe(404);
  expect((await worker.fetch(new Request(url.replace("0123456789abcdef0123456789abcdef", "other")), env)).status).toBe(404);
  expect((await worker.fetch(new Request(url, { method: "DELETE" }), env)).status).toBe(405);
});
