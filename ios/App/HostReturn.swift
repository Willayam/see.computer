import Foundation

/// Sends the user back to the app they were typing in. iOS has no supported way
/// to do this, so this calls an undocumented system class by name. It can stop
/// working in any iOS update and breaks App Store rule 2.5.1, so TestFlight builds
/// leave it out. Whenever this returns false, the app shows the swipe-back screen.
enum HostReturn {
    static func open(bundle: String) -> Bool {
        #if STORE
            return false
        #else
            guard let workspaceClass = NSClassFromString("LSApplicationWorkspace") as? NSObject.Type else {
                return false
            }
            let shared = NSSelectorFromString("defaultWorkspace")
            let open = NSSelectorFromString("openApplicationWithBundleID:")
            guard workspaceClass.responds(to: shared),
                let workspace = workspaceClass.perform(shared)?.takeUnretainedValue() as? NSObject,
                workspace.responds(to: open)
            else { return false }
            return workspace.perform(open, with: bundle) != nil
        #endif
    }
}
