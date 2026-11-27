import AppKit
import Foundation
import WebKit

final class Runner: NSObject, WKNavigationDelegate {
    let view: WKWebView
    let expected: Int
    init(ui: URL, rows: String, expected: Int) {
        self.expected = expected
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.mediaTypesRequiringUserActionForPlayback = []
        let stub = """
          const auditRows = \(rows);
          auditRows.forEach(r => r.revision = 1);
          const file = new Uint8Array(44 + 80000);
          const v = new DataView(file.buffer);
          function tag(at, text) { file.set(new TextEncoder().encode(text), at); }
          tag(0,"RIFF"); v.setUint32(4,file.length-8,true); tag(8,"WAVEfmt "); v.setUint32(16,16,true);
          v.setUint16(20,1,true); v.setUint16(22,1,true); v.setUint32(24,8000,true); v.setUint32(28,8000,true);
          v.setUint16(32,1,true); v.setUint16(34,8,true); tag(36,"data"); v.setUint32(40,80000,true); file.fill(128,44);
          const wav = URL.createObjectURL(new Blob([file], {type:"audio/wav"}));
          const auditHandlers = {};
          window.__TAURI__ = {
            core: { invoke: async (cmd, args) => cmd === 'library_items' ? (args?.known ? {
              versions: Object.fromEntries(auditRows.map(i => [i.id, String(i.revision)])),
              changed: structuredClone(auditRows.filter(i => args.known[i.id] !== String(i.revision)))
            } : auditRows) : cmd === 'dictionary_load' ? {rows:[],stamp:'fixture'} : {history:false},
                    convertFileSrc: () => wav },
            event: { listen: async (name, fn) => { (auditHandlers[name] ||= []).push(fn); } }
          };
          window.auditRefresh = () => Promise.all(auditHandlers['library-changed'].map(fn => fn()));
          """
        config.userContentController.addUserScript(WKUserScript(source: stub, injectionTime: .atDocumentStart, forMainFrameOnly: true))
        view = WKWebView(frame: NSRect(x: 0, y: 0, width: 1040, height: 700), configuration: config)
        super.init()
        view.navigationDelegate = self
        view.loadFileURL(ui.appendingPathComponent("library.html"), allowingReadAccessTo: ui)
    }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        fputs("\(error)\n", stderr)
        exit(1)
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        let select = ProcessInfo.processInfo.environment["SEE_AUDIT_SELECT"] == "1"
        var script = select ? """
          await window.auditRefresh();
          const list = document.querySelector('#list');
          const rowsBefore = list.querySelectorAll('.row[data-id]:not(#words-row)');
          const old = rowsBefore[0].dataset.id;
          const target = rowsBefore[rowsBefore.length - 1];
          if (window.seeLibrary.current() !== old || target.dataset.id === old) throw Error('Unexpected initial selection');
          list.getBoundingClientRect();
          const now = performance.now();
          target.click();
          document.querySelector('#detail').getBoundingClientRect();
          const selection_ms = performance.now() - now;
          const active = list.querySelectorAll('.row.is-on');
          const rowsAfter = [...list.querySelectorAll('.row[data-id]:not(#words-row)')];
          if (active.length !== 1 || active[0].dataset.id !== target.dataset.id || window.seeLibrary.current() !== target.dataset.id) {
            throw Error('Selected row does not match detail');
          }
          const check = [list.innerHTML, document.querySelector('#detail').innerHTML, document.querySelector('#count').textContent].join(' ');
          let digest = 2166136261;
          for (let i = 0; i < check.length; i++) digest = Math.imul(digest ^ check.charCodeAt(i), 16777619);
          return {rows: rowsAfter.length, visible_lines: document.querySelectorAll('#detail [data-idx].line').length,
                  selection_ms, selected: active[0].dataset.id, dom_checksum: digest >>> 0,
                  rebuilt_rows: rowsAfter.filter((row, i) => rowsBefore[i] !== row).length};
          """ : """
          await window.auditRefresh();
          const list = document.querySelector('#list');
          const before = document.querySelector('#detail audio');
          list.getBoundingClientRect();
          const now = performance.now();
          await window.auditRefresh();
          list.getBoundingClientRect();
          const rows = list.querySelectorAll('.row[data-id]:not(#words-row)').length;
          const after = document.querySelector('#detail audio');
          return {rows, refresh_ms: performance.now()-now,
                  visible_lines: document.querySelectorAll('#detail [data-idx].line').length,
                  audio_replaced: !!before && !!after && before !== after && !before.isConnected};
          """
        if let file = ProcessInfo.processInfo.environment["SEE_AUDIT_SCRIPT"] {
            do { script = try String(contentsOfFile: file, encoding: .utf8) }
            catch { fputs("Failed to read the requested navigation test.\n", stderr); exit(1) }
        }
        webView.callAsyncJavaScript(script, arguments: [:], in: nil, in: .page) { result in
            do {
                guard let data = try result.get() as? [String: Any], let rows = data["rows"] as? Int, rows == self.expected else {
                    throw NSError(domain: "Missing Library rows", code: 1)
                }
                print(String(data: try JSONSerialization.data(withJSONObject: data, options: [.sortedKeys]), encoding: .utf8)!)
                exit(0)
            } catch {
                fputs("\(error)\n", stderr)
                exit(1)
            }
        }
    }
}

let args = CommandLine.arguments
let ui = URL(fileURLWithPath: args[1], isDirectory: true)
let rows = try String(contentsOfFile: args[2], encoding: .utf8)
let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let runner = Runner(ui: ui, rows: rows, expected: Int(args[3])!)
let window = NSWindow(contentRect: runner.view.frame, styleMask: [.titled], backing: .buffered, defer: false)
window.contentView = runner.view
window.orderFrontRegardless()
DispatchQueue.main.asyncAfter(deadline: .now() + 35) {
    fputs("WebKit fixture timed out\n", stderr)
    exit(1)
}
withExtendedLifetime(runner) { app.run() }
