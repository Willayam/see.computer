import AppKit
import WebKit

final class PillProbe: NSObject, WKNavigationDelegate {
    let webView: WKWebView

    init(page: URL) {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        let bridge = """
            window.listeners = {};
            window.__TAURI__ = {event:{listen: async (name, cb) => listeners[name] = cb, emit: async () => {}}};
            window.queuedFrames = [];
            window.requestAnimationFrame = cb => (queuedFrames.push(cb), queuedFrames.length);
            window.canvasClears = window.canvasFills = 0;
            const drawing = CanvasRenderingContext2D.prototype;
            const clear = drawing.clearRect, fill = drawing.fill;
            drawing.clearRect = function(...args) { canvasClears++; return clear.apply(this, args) };
            drawing.fill = function(...args) { canvasFills++; return fill.apply(this, args) };
            """
        config.userContentController.addUserScript(WKUserScript(
            source: bridge, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        webView = WKWebView(frame: NSRect(x: 0, y: 0, width: 520, height: 150), configuration: config)
        super.init()
        webView.navigationDelegate = self
        webView.loadFileURL(page, allowingReadAccessTo: page.deletingLastPathComponent())
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        let script = """
            let now = performance.now();
            function tick(n) {
                for (let i = 0; i < n; i++) {
                    now += 1000 / 60;
                    if (queuedFrames.length > 1) throw Error('Duplicate animation loops');
                    queuedFrames.shift()?.(now);
                }
            }
            function send(payload) { listeners.pill({payload}); }
            tick(600);
            if (canvasClears || canvasFills || queuedFrames.length) throw Error('Idle canvas redrew');
            send({kind:'show', activity:'listening'});
            tick(60);
            if (!canvasFills) throw Error('Listening did not draw');
            send({kind:'hide'});
            tick(120);
            const settled = canvasClears;
            tick(600);
            if (canvasClears !== settled || queuedFrames.length) throw Error('Hidden canvas still draws');
            const before = canvasFills;
            send({kind:'show', activity:'listening'});
            tick(60);
            if (canvasFills <= before) throw Error('Next take failed to wake the pill');
            return 'Idle and post-fade each cleared zero frames across 600 ticks. Both listening takes drew.';
            """
        webView.callAsyncJavaScript(script, arguments: [:], in: nil, in: .page) { response in
            switch response {
            case let .success(message):
                print(message)
                exit(0)
            case let .failure(error):
                fputs("\(error)\n", stderr)
                exit(1)
            }
        }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let probe = PillProbe(page: URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: false))
DispatchQueue.main.asyncAfter(deadline: .now() + 10) { exit(2) }
withExtendedLifetime(probe) { app.run() }
