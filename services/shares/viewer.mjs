const allowedAsset = /^[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*(?:\/[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*)*\.(?:png|jpg|mp4|mov|m4s|m3u8|json|md)$/;

export function viewerHtml(nonce) {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Shared screen take</title>
  <style>
    :root { color-scheme: light dark; font: 16px/1.5 system-ui, sans-serif; }
    body { max-width: 58rem; margin: 0 auto; padding: 2rem 1rem 5rem; }
    header { border-bottom: 1px solid color-mix(in srgb, currentColor 20%, transparent); margin-bottom: 2rem; }
    h1, h2 { line-height: 1.15; }
    .status { color: color-mix(in srgb, currentColor 72%, transparent); }
    .narration { white-space: pre-wrap; font-size: 1.1rem; }
    .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(17rem, 1fr)); gap: 1rem; }
    figure { margin: 0; }
    img, video { display: block; width: 100%; max-height: 34rem; object-fit: contain; background: #111; border-radius: .4rem; }
    figcaption, .note { margin-top: .4rem; font-size: .9rem; }
    a { color: inherit; }
    [hidden] { display: none !important; }
  </style>
</head>
<body>
  <header>
    <h1>Shared screen take</h1>
    <p id="status" class="status" role="status" aria-live="polite">Checking the upload...</p>
  </header>
  <main>
    <section aria-labelledby="narration-heading">
      <h2 id="narration-heading">Narration</h2>
      <p id="narration" class="narration">The transcript is not ready yet.</p>
    </section>
    <section aria-labelledby="captures-heading">
      <h2 id="captures-heading">Captures</h2>
      <div id="captures" class="grid"></div>
    </section>
    <section aria-labelledby="files-heading">
      <h2 id="files-heading">Files for agents</h2>
      <p><a href="take.md?absolute=1">take.md with absolute media links</a></p>
      <p><a href="transcript.json">transcript.json</a></p>
    </section>
  </main>
  <script nonce="${nonce}" src="/hls-1.7.2.min.js"></script>
  <script nonce="${nonce}">
    const allowedAsset = ${allowedAsset.toString()};
    const root = new URL("./", location.href);
    const statusNode = document.querySelector("#status");
    const narrationNode = document.querySelector("#narration");
    const capturesNode = document.querySelector("#captures");
    let renderedFinal = false;
    let liveVersion = "";
    let players = [];

    function clearPlayers() {
      for (const player of players) player.destroy();
      players = [];
    }

    function assetPath(value) {
      if (typeof value !== "string" || value.length > 128 || !allowedAsset.test(value)) return null;
      const url = new URL(value, root);
      return url.origin === root.origin && url.pathname.startsWith(root.pathname) ? url : null;
    }

    function note(text) {
      const node = document.createElement("p");
      node.className = "note";
      node.textContent = text;
      return node;
    }

    function imageFigure(path, label) {
      const url = assetPath(path);
      if (!url || !/\.(?:png|jpg)$/.test(url.pathname)) return null;
      const figure = document.createElement("figure");
      const image = document.createElement("img");
      image.src = url.href;
      image.alt = label;
      image.loading = "lazy";
      figure.append(image);
      const caption = document.createElement("figcaption");
      caption.textContent = label;
      figure.append(caption);
      return figure;
    }

    function videoFigure(path, label, live) {
      const url = assetPath(path);
      if (!url || !/\.(?:mp4|mov|m3u8)$/.test(url.pathname)) return null;
      const figure = document.createElement("figure");
      const video = document.createElement("video");
      video.controls = true;
      video.playsInline = true;
      video.preload = live ? "none" : "metadata";
      if (url.pathname.endsWith(".m3u8") && !video.canPlayType("application/vnd.apple.mpegurl")) {
        if (window.Hls && Hls.isSupported()) {
          const hls = new Hls({ enableWorker: false, maxBufferLength: 15, backBufferLength: 15 });
          hls.loadSource(url.href);
          hls.attachMedia(video);
          players.push(hls);
          figure.append(video);
        } else {
          figure.append(note("This browser cannot play the clip. Open its link in a media player."));
        }
      } else {
        video.src = url.href;
        figure.append(video);
      }
      const link = document.createElement("a");
      link.href = url.href;
      link.textContent = label;
      const caption = document.createElement("figcaption");
      caption.append(link);
      figure.append(caption);
      return figure;
    }

    function renderLive(manifest) {
      if (!manifest || typeof manifest !== "object") return;
      const version = JSON.stringify(manifest);
      if (version === liveVersion) return;
      liveVersion = version;
      clearPlayers();
      const next = [];
      if (Array.isArray(manifest.shots)) {
        for (const shot of manifest.shots) {
          const figure = imageFigure(shot && shot.file, "Screenshot recorded during upload");
          if (figure) next.push(figure);
        }
      }
      if (Array.isArray(manifest.clips)) {
        for (const clip of manifest.clips) {
          const figure = videoFigure(clip && clip.playlist, "Live video clip", true);
          if (figure) next.push(figure);
        }
      }
      capturesNode.replaceChildren(...next);
    }

    async function renderFinal(transcript) {
      if (!transcript || typeof transcript !== "object") throw new Error("Invalid transcript");
      narrationNode.textContent = typeof transcript.fullText === "string" && transcript.fullText ? transcript.fullText : "No speech was detected.";
      clearPlayers();
      const next = [];
      if (Array.isArray(transcript.captures)) {
        for (const capture of transcript.captures) {
          if (!capture || typeof capture !== "object") continue;
          if (capture.type === "shot") {
            const figure = imageFigure(capture.file, typeof capture.timestamp === "string" ? "Screenshot at " + capture.timestamp : "Screenshot");
            if (figure) next.push(figure);
          }
          if (capture.type === "clip") {
            const figure = videoFigure(capture.video, typeof capture.durationMs === "number" ? "Video clip, " + Math.round(capture.durationMs / 1000) + " seconds" : "Video clip", false);
            if (figure) next.push(figure);
            if (Array.isArray(capture.frames)) {
              for (const frame of capture.frames) {
                const frameFigure = imageFigure(frame && frame.file, frame && typeof frame.timestamp === "string" ? "Video frame at " + frame.timestamp : "Video frame");
                if (frameFigure) next.push(frameFigure);
              }
            }
          }
        }
      }
      if (!next.some((figure) => figure.querySelector('a[href$="fullclip.mp4"]'))) {
        const packagedUrl = assetPath("fullclip.mp4");
        if (packagedUrl) {
          try {
            const packaged = await fetch(packagedUrl, { method: "HEAD", cache: "no-store" });
            if (packaged.ok) {
              const figure = videoFigure("fullclip.mp4", "Packaged video", false);
              if (figure) next.unshift(figure);
            }
          } catch {}
        }
      }
      capturesNode.replaceChildren(...next);
      renderedFinal = true;
    }

    async function json(path) {
      const response = await fetch(path, { cache: "no-store" });
      if (!response.ok) throw new Error("Request failed");
      return response.json();
    }

    async function refresh() {
      try {
        const status = await json("status.json");
        const expires = new Date(status.expiresAt).toLocaleString();
        if (status.state === "ready") {
          statusNode.textContent = "Upload complete. This link expires " + expires + ".";
          if (!renderedFinal) await renderFinal(await json("transcript.json"));
          return;
        }
        statusNode.textContent = "Upload in progress. This page will update. The link expires " + expires + ".";
        try { renderLive(await json("live.json")); } catch {}
        setTimeout(refresh, 2000);
      } catch {
        statusNode.textContent = "This share is unavailable.";
        setTimeout(refresh, 5000);
      }
    }
    refresh();
  </script>
</body>
</html>`;
}

export function rootHtml() {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>see.computer sharing beta</title>
  <style>
    :root { color-scheme: light dark; font: 16px/1.55 system-ui, sans-serif; }
    body { max-width: 42rem; margin: 0 auto; padding: 3rem 1rem; }
    h1 { line-height: 1.15; }
    code { overflow-wrap: anywhere; }
  </style>
</head>
<body>
  <main>
    <h1>see.computer sharing beta</h1>
    <p>Keep transcription on your Mac. Turn on <strong>Make recordings shareable</strong> in the see.computer beta to send screenshots and video clips to a link you can open anywhere.</p>
    <p>Anyone with the link can see the recording and transcript. Sharing is off by default. Turning it off pauses pending uploads.</p>
    <p>The beta allows 10 new shares per installation each UTC month, up to 20 from one IP address each UTC day. Total beta capacity is 100 new shares per UTC day.</p>
    <p>Each share can hold 256 MiB across 1,500 files. One file can be at most 8 MiB. Links expire within 30 days. Canceled and empty shares can still count against creation limits.</p>
    <ol>
      <li>Turn on sharing in the app's Settings.</li>
      <li>Capture a screenshot or video as usual. Uploading starts during the recording.</li>
      <li>When you finish, the app pastes your words and a share link.</li>
      <li>Use <strong>Remove last shared link</strong> in Settings to revoke the latest link.</li>
    </ol>
    <p>Uploads resume after connection failures. The original files remain on your Mac.</p>
    <p><a href="https://github.com/Willayam/see.computer">App source and releases</a></p>
  </main>
</body>
</html>`;
}
