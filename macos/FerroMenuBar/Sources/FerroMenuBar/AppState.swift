import AppKit
import Foundation

enum Health: String {
    case ok, working, problem, offline

    var icon: NSImage {
        let color: NSColor = switch self {
        case .ok: .systemGreen
        case .working: .systemYellow
        case .problem: .systemRed
        case .offline: .systemGray
        }
        let size = NSSize(width: 18, height: 18)
        let image = NSImage(size: size, flipped: false) { rect in
            color.setFill()
            NSBezierPath(ovalIn: rect.insetBy(dx: 2.5, dy: 2.5)).fill()
            return true
        }
        image.isTemplate = false
        return image
    }

    var label: String {
        switch self {
        case .ok: return "all systems go"
        case .working: return "working…"
        case .problem: return "needs attention"
        case .offline: return "daemon not running"
        }
    }
}

@MainActor
final class AppState: ObservableObject {
    @Published var sites: [Site] = []
    @Published var services: [ServiceInfo] = []
    @Published var conflicts: [ConflictInfo] = []
    @Published var httpsPort = 8443
    @Published var daemonUp = false

    private let events = EventStream()
    private var refreshTask: Task<Void, Never>?
    private var started = false

    nonisolated static let appData = NSHomeDirectory() + "/Library/Application Support/ferro"
    nonisolated static let logsDir = NSHomeDirectory() + "/Library/Logs/ferro"
    nonisolated static let ferroBin = NSHomeDirectory() + "/.ferro/bin/ferro"

    var health: Health {
        if !daemonUp { return .offline }
        if !conflicts.isEmpty { return .problem }
        // Crash-looping or repeatedly restarted services are a problem.
        if services.contains(where: { $0.status == "backoff" }) { return .problem }
        if services.contains(where: { ($0.restarts ?? 0) > 3 }) { return .problem }
        // "stopped" is a normal idle state (e.g. a PHP version no site
        // currently uses); only "starting" means work is in flight.
        if services.contains(where: { $0.status == "starting" }) { return .working }
        return .ok
    }

    var mysqlPort: Int {
        let config = Self.appData + "/config.toml"
        guard let text = try? String(contentsOfFile: config) else { return 3307 }
        for line in text.split(separator: "\n") {
            let parts = line.replacingOccurrences(of: " ", with: "").split(separator: "=")
            if parts.count == 2, parts[0] == "mysql_port", let port = Int(parts[1]) {
                return port
            }
        }
        return 3307
    }

    /// First parked folder — where `ferro new` puts sites unless a
    /// custom location is chosen.
    var defaultSitesDir: String {
        let fallback = NSHomeDirectory() + "/Sites"
        let config = Self.appData + "/config.toml"
        guard let text = try? String(contentsOfFile: config) else { return fallback }
        for line in text.split(separator: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard trimmed.hasPrefix("parked_paths"),
                  let eq = trimmed.firstIndex(of: "=") else { continue }
            let value = trimmed[trimmed.index(after: eq)...]
                .replacingOccurrences(of: "[", with: "")
                .replacingOccurrences(of: "]", with: "")
            guard let first = value.split(separator: ",").first else { continue }
            let path = first.trimmingCharacters(in: .whitespaces)
                .trimmingCharacters(in: CharacterSet(charactersIn: "\""))
            if !path.isEmpty { return path }
        }
        return fallback
    }

    var phpVersions: [String] {
        let dir = Self.appData + "/bin/php"
        let entries = (try? FileManager.default.contentsOfDirectory(atPath: dir)) ?? []
        return entries.filter { $0.first?.isNumber == true }.sorted()
    }

    // MARK: - bundled dependencies

    struct DependencyStatus: Identifiable {
        var id: String { component }
        let component: String
        let title: String
        let detail: String
        let installed: Bool
    }

    /// MySQL and at least one PHP are the minimum a site needs to run.
    var coreDependenciesMissing: Bool {
        !FileManager.default.fileExists(atPath: Self.appData + "/bin/mysql/bin/mysqld")
            || phpVersions.isEmpty
    }

    /// Installed/missing state of every bundled component, derived from
    /// the files `ferro update` fetches.
    func dependencyStatuses() -> [DependencyStatus] {
        let fm = FileManager.default
        let bin = Self.appData + "/bin"
        func exists(_ path: String) -> Bool {
            fm.fileExists(atPath: path)
        }
        let supported = ["8.1", "8.2", "8.3", "8.4"]
        let present = supported.filter { exists(bin + "/php/\($0)/php-fpm") }
        let cacheDir = Self.appData + "/cache/wordpress"
        let cachedCores = (try? fm.contentsOfDirectory(atPath: cacheDir))?
            .filter { !$0.hasPrefix(".") } ?? []
        return [
            DependencyStatus(
                component: "php",
                title: "PHP 8.1–8.4 (static, with php-fpm)",
                detail: present.isEmpty
                    ? "not installed"
                    : "installed: " + present.joined(separator: ", ")
                        + (present.count < supported.count ? " (Install fetches all)" : ""),
                installed: !present.isEmpty
            ),
            DependencyStatus(
                component: "mysql",
                title: "MySQL 8.4 server",
                detail: exists(bin + "/mysql/bin/mysqld") ? "installed" : "not installed",
                installed: exists(bin + "/mysql/bin/mysqld")
            ),
            DependencyStatus(
                component: "wp-cli",
                title: "WP-CLI",
                detail: exists(bin + "/wp-cli.phar") ? "installed" : "not installed",
                installed: exists(bin + "/wp-cli.phar")
            ),
            DependencyStatus(
                component: "composer",
                title: "Composer",
                detail: exists(bin + "/composer.phar") ? "installed" : "not installed",
                installed: exists(bin + "/composer.phar")
            ),
            DependencyStatus(
                component: "adminer",
                title: "Adminer (database UI)",
                detail: exists(Self.appData + "/adminer/adminer.php") ? "installed" : "not installed",
                installed: exists(Self.appData + "/adminer/adminer.php")
            ),
            DependencyStatus(
                component: "mailpit",
                title: "Mailpit (mail catcher)",
                detail: exists(bin + "/mailpit") ? "installed" : "not installed",
                installed: exists(bin + "/mailpit")
            ),
            DependencyStatus(
                component: "wordpress-cache",
                title: "WordPress core cache",
                detail: cachedCores.isEmpty
                    ? "empty — downloaded on first site"
                    : "cached: " + cachedCores.joined(separator: ", "),
                installed: !cachedCores.isEmpty
            ),
        ]
    }

    func start() {
        guard !started else { return }
        started = true
        refresh()
        // Live updates: any daemon event triggers one debounced refresh
        // (no polling).
        events.start { [weak self] _ in
            Task { @MainActor in self?.scheduleRefresh() }
        }
    }

    func scheduleRefresh() {
        refreshTask?.cancel()
        refreshTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard !Task.isCancelled else { return }
            await self.load()
        }
    }

    func refresh() {
        Task { @MainActor in await load() }
    }

    private func load() async {
        do {
            async let statusTask = Task.detached {
                try DaemonClient.callDecodable("status", as: StatusReport.self)
            }.value
            async let sitesTask = Task.detached {
                try DaemonClient.callDecodable("site.list", as: [Site].self)
            }.value
            let (report, list) = try await (statusTask, sitesTask)
            services = report.services
            conflicts = report.conflicts
            httpsPort = report.https_port
            sites = list
            daemonUp = true
        } catch {
            daemonUp = false
        }
    }

    private func ipc(_ method: String, _ params: [String: Any] = [:]) {
        guard let data = try? JSONSerialization.data(withJSONObject: params) else { return }
        Task.detached {
            _ = try? DaemonClient.call(method, paramsJSON: data)
            await self.scheduleRefresh()
        }
    }

    // MARK: - site actions

    func url(_ site: Site) -> URL {
        URL(string: "https://\(site.name).localhost:\(httpsPort)")!
    }

    func open(_ site: Site) {
        NSWorkspace.shared.open(url(site))
    }

    /// One-click admin login: single-use token from the daemon, consumed
    /// by the mu-plugin on the site.
    func openAdmin(_ site: Site) {
        Task.detached {
            guard let result = try? DaemonClient.call("login.create", ["name": site.name]),
                  let token = (result as? [String: Any])?["token"] as? String else { return }
            let port = await self.httpsPort
            let url = URL(string: "https://\(site.name).localhost:\(port)/?ferro-login=\(token)")!
            await NSWorkspace.shared.open(url)
        }
    }

    func openDatabase(_ site: Site) {
        let url = URL(string: "https://\(site.name).localhost:\(httpsPort)/__adminer/")!
        NSWorkspace.shared.open(url)
    }

    func openTerminal(_ site: Site) {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/open")
        task.arguments = ["-a", "Terminal", site.path]
        try? task.run()
    }

    func reveal(_ site: Site) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: site.path)])
    }

    func openLogs(_ site: Site) {
        let log = Self.logsDir + "/\(site.name)-php.log"
        NSWorkspace.shared.open(URL(fileURLWithPath: log))
    }

    func copyDBInfo(_ site: Site) {
        let info = """
        host: 127.0.0.1
        port: \(mysqlPort)
        database: \(site.db_name ?? "ferro_\(site.name.replacingOccurrences(of: "-", with: "_"))")
        user: \(site.db_user ?? site.db_name ?? "")
        password: see `ferro db \(site.name) info --show-password`
        """
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(info, forType: .string)
    }

    func setPHP(_ site: Site, _ version: String) {
        ipc("site.set_php", ["name": site.name, "version": version])
    }

    func setRunning(_ site: Site, _ running: Bool) {
        ipc("site.set_running", ["name": site.name, "running": running])
    }

    /// Clone runs through the CLI (the daemon has no clone RPC).
    func clone(_ site: Site) {
        guard let newName = prompt(
            title: "Clone \(site.name)",
            message: "Name for the copy (files + database, URLs rewritten):",
            placeholder: "\(site.name)-copy"
        ) else { return }
        runFerro(["clone", site.name, newName])
    }

    func destroy(_ site: Site) {
        let alert = NSAlert()
        alert.messageText = "Delete \(site.name)?"
        alert.informativeText =
            "Drops its database and MySQL user, removes its config, and moves \(site.path) to the Trash."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Move to Trash")
        alert.addButton(withTitle: "Cancel")
        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        runFerro(["destroy", site.name, "--yes"])
    }

    func runFerro(_ args: [String]) {
        Task.detached {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: Self.ferroBin)
            process.arguments = args
            try? process.run()
            process.waitUntilExit()
            await self.scheduleRefresh()
        }
    }

    /// Start the launchd agent (installs the plist on first run).
    func startDaemon() {
        runFerro(["daemon", "start"])
    }

    /// Stop the daemon (which stops MySQL, php-fpm and Mailpit
    /// gracefully), then quit the menu bar app.
    func stopAllAndQuit() {
        Task.detached {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: Self.ferroBin)
            process.arguments = ["daemon", "stop"]
            try? process.run()
            process.waitUntilExit()
            await NSApplication.shared.terminate(nil)
        }
    }

    func prompt(title: String, message: String, placeholder: String) -> String? {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 24))
        field.placeholderString = placeholder
        alert.accessoryView = field
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return nil }
        let value = field.stringValue.trimmingCharacters(in: .whitespaces)
        return value.isEmpty ? nil : value
    }
}
