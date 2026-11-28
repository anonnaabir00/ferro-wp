import SwiftUI

@main
struct FerroMenuBarApp: App {
    @StateObject private var state = AppState()

    var body: some Scene {
        MenuBarExtra {
            MenuContent()
                .environmentObject(state)
                .onAppear { state.refresh() }
        } label: {
            Image(nsImage: state.health.icon)
                .accessibilityLabel("Ferro WP: \(state.health.label)")
                .onAppear { state.start() }
        }
        .menuBarExtraStyle(.menu)

        Window("New Site — Ferro WP", id: "new-site") {
            NewSiteView()
                .environmentObject(state)
        }
        .defaultLaunchBehavior(.suppressed)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)

        Window("Pull Site — Ferro WP", id: "pull-site") {
            PullSiteView()
                .environmentObject(state)
        }
        .defaultLaunchBehavior(.suppressed)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)

        Window("Add Existing Site — Ferro WP", id: "add-site") {
            AddExistingSiteView()
                .environmentObject(state)
        }
        .defaultLaunchBehavior(.suppressed)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)

        Window("Dependencies — Ferro WP", id: "dependencies") {
            DependenciesView()
                .environmentObject(state)
        }
        .defaultLaunchBehavior(.suppressed)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)

        Window("Doctor — Ferro WP", id: "doctor") {
            DoctorView()
        }
        .defaultLaunchBehavior(.suppressed)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)

        Window("Settings — Ferro WP", id: "settings") {
            SettingsView()
                .environmentObject(state)
        }
        .defaultLaunchBehavior(.suppressed)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)
    }
}

/// MenuBarExtra apps are LSUIElement accessories: `openWindow` alone creates
/// the window behind the currently active app, so it looks like nothing
/// happened. Activate the app, then order the window front once SwiftUI
/// has materialized it.
@MainActor
func openAndFocus(_ openWindow: OpenWindowAction, id: String, title: String) {
    openWindow(id: id)
    Task {
        for _ in 0..<20 {
            NSApp.activate()
            if let window = NSApp.windows.first(where: {
                $0.identifier?.rawValue == id || $0.title == title
            }) {
                window.makeKeyAndOrderFront(nil)
                return
            }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
    }
}

struct MenuContent: View {
    @EnvironmentObject var state: AppState
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Text("Ferro WP — \(state.health.label)")

        if !state.conflicts.isEmpty {
            ForEach(state.conflicts) { conflict in
                Text("⚠ :\(conflict.port) held by PID \(conflict.pid)")
            }
        }

        if !state.daemonUp {
            Button("Start Daemon") { state.startDaemon() }
        }

        if state.daemonUp && state.coreDependenciesMissing {
            Button("⚠ Missing Dependencies…") {
                openAndFocus(openWindow, id: "dependencies", title: "Dependencies — Ferro WP")
            }
        }

        Divider()

        if state.sites.isEmpty {
            Text("no sites yet")
        } else {
            ForEach(state.sites) { site in
                SiteMenu(site: site)
            }
        }

        Divider()

        Button("New Site…") {
            openAndFocus(openWindow, id: "new-site", title: "New Site — Ferro WP")
        }
        Button("Add Existing Site…") {
            openAndFocus(openWindow, id: "add-site", title: "Add Existing Site — Ferro WP")
        }
        Button("Pull Site…") {
            openAndFocus(openWindow, id: "pull-site", title: "Pull Site — Ferro WP")
        }

        Divider()

        Button("Dependencies…") {
            openAndFocus(openWindow, id: "dependencies", title: "Dependencies — Ferro WP")
        }
        Button("Doctor…") {
            openAndFocus(openWindow, id: "doctor", title: "Doctor — Ferro WP")
        }
        Button("Settings…") {
            openAndFocus(openWindow, id: "settings", title: "Settings — Ferro WP")
        }

        Divider()

        Button("Stop Everything & Quit") { state.stopAllAndQuit() }
        Button("Quit Menu Bar (daemon keeps running)") {
            NSApplication.shared.terminate(nil)
        }
    }
}

struct SiteMenu: View {
    @EnvironmentObject var state: AppState
    let site: Site

    var body: some View {
        Menu(site.running ? site.name : "\(site.name) (stopped)") {
            Button("Open") { state.open(site) }
            Button("Open Admin") { state.openAdmin(site) }
            Button("Open Database") { state.openDatabase(site) }
            Button("Open Terminal here") { state.openTerminal(site) }
            Button("Copy DB Info") { state.copyDBInfo(site) }

            Menu("PHP \(site.php_version)") {
                ForEach(state.phpVersions, id: \.self) { version in
                    Button(version == site.php_version ? "\(version) ✓" : version) {
                        state.setPHP(site, version)
                    }
                    .disabled(version == site.php_version)
                }
            }

            Button("Logs") { state.openLogs(site) }

            if site.running {
                Button("Stop") { state.setRunning(site, false) }
            } else {
                Button("Start") { state.setRunning(site, true) }
            }

            Button("Reveal in Finder") { state.reveal(site) }
            Button("Clone…") { state.clone(site) }

            Divider()

            Button("Delete…") { state.destroy(site) }
        }
    }
}
