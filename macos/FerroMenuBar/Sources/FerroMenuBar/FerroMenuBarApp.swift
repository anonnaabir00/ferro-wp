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

        Divider()

        if state.sites.isEmpty {
            Text("no sites yet")
        } else {
            ForEach(state.sites) { site in
                SiteMenu(site: site)
            }
        }

        Divider()

        Button("New Site…") { openWindow(id: "new-site") }
        Button("Pull Site…") { openWindow(id: "pull-site") }
        Button("Adminer") { state.openAdminer() }

        Divider()

        Button("Doctor…") { openWindow(id: "doctor") }
        Button("Settings…") { openWindow(id: "settings") }

        Divider()

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
