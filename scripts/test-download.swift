import AppKit
import WebKit

final class DownloadProbe: NSObject, WKNavigationDelegate {
    let view: WKWebView

    init(folder: URL) {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.userContentController.addUserScript(WKUserScript(source: """
            const row = document.querySelector('.hero .cta-row');
            const link = row?.querySelector('a');
            window.downloadVisible = !!link && link.getAttribute('href') === '/download/mac'
                && getComputedStyle(row).opacity === '1' && link.getBoundingClientRect().width > 32;
            """, injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        view = WKWebView(frame: NSRect(x: 0, y: 0, width: 390, height: 800), configuration: config)
        super.init()
        view.navigationDelegate = self
        view.loadFileURL(folder.appendingPathComponent("index.html"), allowingReadAccessTo: folder)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        webView.evaluateJavaScript("window.downloadVisible") { value, error in
            guard error == nil, value as? Bool == true else {
                fputs("Download was hidden when family.js was missing.\n", stderr)
                exit(1)
            }
            print("Download was visible on first render without family.js.")
            exit(0)
        }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let probe = DownloadProbe(folder: URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true))
DispatchQueue.main.asyncAfter(deadline: .now() + 15) { exit(2) }
withExtendedLifetime(probe) { app.run() }
