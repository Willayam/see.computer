import AppKit
import WebKit

final class PillProbe: NSObject, WKNavigationDelegate {
    let webView: WKWebView

    init(page: URL) {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        let bridge = """
            window.listeners = {};
            window.actions = [];
            window.__TAURI__ = {event:{
                listen: async (name, cb) => listeners[name] = cb,
                emit: async (name, payload) => { if (name === 'pill-action') actions.push(payload); }
            }};
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
            const copy = document.getElementById('ccopy');
            const reply = (id, ok) => listeners['pill-copy-result']({payload:[id, ok]});
            send({kind:'held', id:41, text:'First take'});
            copy.click();
            send({kind:'held', id:42, text:'Next take'});
            reply(41, true);
            if (copy.textContent !== 'Copy') throw Error('Previous reply claimed the next take');
            copy.click();
            reply(42, false);
            if (copy.textContent !== 'Retry') throw Error('Failed copy did not offer a retry');
            copy.click();
            reply(42, true);
            if (copy.textContent !== 'Copied') throw Error('Successful retry was not shown');
            send({kind:'held', id:43, text:'Dismiss this take'});
            document.getElementById('cx').click();
            const actual = actions.map(({action,id}) => [action,id]);
            const expected = [['copy',41],['copy',42],['copy',42],['discard',43]];
            if (JSON.stringify(actual) !== JSON.stringify(expected)) throw Error('Wrong take: ' + JSON.stringify(actual));
            return 'Idle drawing stopped, listening resumed, and only the selected take could be copied or discarded.';
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
