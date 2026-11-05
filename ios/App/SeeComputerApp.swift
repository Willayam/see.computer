import SwiftUI

@main
struct SeeComputerApp: App {
    @StateObject private var session: Session
    #if DEBUG
        private static let bench = Bench()
    #endif

    init() {
        #if DEBUG
            if Bench.isRequested {
                Self.bench.start()
                _session = StateObject(wrappedValue: Session(benching: true))
                return
            }
        #endif
        _session = StateObject(wrappedValue: Session(benching: false))
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(session)
                .onOpenURL { url in
                    guard url == Bridge.armURL else { return }
                    session.openedFromKeyboard()
                }
                .tint(Color(red: 0.353, green: 0.784, blue: 0.980))
        }
    }
}
