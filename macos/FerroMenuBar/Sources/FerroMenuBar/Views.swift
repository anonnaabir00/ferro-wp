import ServiceManagement
import SwiftUI

// MARK: - ferro CLI runner with live output

@MainActor
final class FerroProcess: ObservableObject {
    @Published var output = ""
    @Published var running = false
    @Published var exitCode: Int32?

    private var process: Process?

    var succeeded: Bool { exitCode == 0 }

    func run(_ args: [String]) {
        output = ""
        exitCode = nil
        running = true
        let process = Process()
        let pipe = Pipe()
        process.executableURL = URL(fileURLWithPath: AppState.ferroBin)
        process.arguments = args
        process.standardOutput = pipe
        process.standardError = pipe
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = NSHomeDirectory() + "/.ferro/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        process.environment = env
        pipe.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            guard !data.isEmpty else { return }
            Task { @MainActor in self.output += String(decoding: data, as: UTF8.self) }
        }
        process.terminationHandler = { process in
            Task { @MainActor in
                self.running = false
                self.exitCode = process.terminationStatus
            }
        }
        do {
            try process.run()
            self.process = process
        } catch {
            output += "could not start ferro: \(error.localizedDescription)\n"
            running = false
            exitCode = -1
        }
    }
}

struct OutputLog: View {
    let text: String

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                Text(text)
                    .font(.system(.caption, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
                    .padding(8)
                    .id("bottom")
            }
            .background(Color(nsColor: .textBackgroundColor))
            .onChange(of: text) { proxy.scrollTo("bottom") }
        }
    }
}

// MARK: - New Site

struct NewSiteView: View {
    @EnvironmentObject var state: AppState
    @StateObject private var job = FerroProcess()

    @State private var name = ""
    @State private var php = ""
    @State private var wp = "latest"
    @State private var title = ""
    @State private var adminUser = "admin"
    @State private var adminEmail = ""
    @State private var multisite = false
    @State private var blueprint = ""
    @State private var location = ""
    @State private var password = ""

    private var validName: Bool {
        !name.isEmpty
            && name.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") }
            && name == name.lowercased()
            && !name.hasPrefix("-") && !name.hasSuffix("-")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Form {
                TextField("Name:", text: $name)
                    .onChange(of: name) { name = name.lowercased() }
                Text(validName
                     ? "https://\(name).localhost:\(state.httpsPort)"
                     : "letters, digits and hyphens")
                    .font(.caption)
                    .foregroundStyle(validName ? .green : .secondary)
                Picker("PHP:", selection: $php) {
                    ForEach(state.phpVersions, id: \.self) { Text($0).tag($0) }
                }
                TextField("WordPress:", text: $wp)
                TextField("Title (optional):", text: $title)
                TextField("Admin user:", text: $adminUser)
                TextField("Admin email (optional):", text: $adminEmail)
                Toggle("Multisite (subdirectory)", isOn: $multisite)
                HStack {
                    TextField("Location (optional):", text: $location,
                              prompt: Text(state.defaultSitesDir))
                    Button("Choose…") { pickLocation() }
                }
                Text("\(effectiveLocation)/\(name.isEmpty ? "<name>" : name)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                HStack {
                    TextField("Blueprint (optional):", text: $blueprint)
                    Button("Choose…") { pickBlueprint() }
                }
            }

            if job.running || job.exitCode != nil {
                OutputLog(text: job.output)
                    .frame(minHeight: 140)
            }

            if job.succeeded {
                if !password.isEmpty {
                    HStack {
                        Text("Admin password: \(password)")
                            .font(.caption)
                            .textSelection(.enabled)
                        Button("Copy") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(password, forType: .string)
                        }
                    }
                    Text("Stored in the Keychain — shown here once.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                HStack {
                    Button("Open Site") {
                        state.open(Site(name: name, path: "", php_version: php, running: true,
                                        db_name: nil, db_user: nil, db_pass: nil))
                    }
                    Button("Open Admin") {
                        state.openAdmin(Site(name: name, path: "", php_version: php, running: true,
                                             db_name: nil, db_user: nil, db_pass: nil))
                    }
                }
            }

            HStack {
                Spacer()
                if job.running { ProgressView().scaleEffect(0.7) }
                Button(job.succeeded ? "Create Another" : "Create Site") {
                    password = ""
                    job.run(arguments)
                }
                .disabled(!validName || job.running)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding()
        .frame(width: 460)
        .onAppear {
            if php.isEmpty { php = state.phpVersions.last ?? "8.3" }
            state.refresh()
        }
        .onChange(of: job.exitCode) {
            if job.exitCode == 0 { fetchPassword() }
        }
    }

    /// The CLI generated a password and stored it in the Keychain;
    /// show it once with a copy button.
    private func fetchPassword() {
        let siteName = name
        Task.detached {
            let process = Process()
            let pipe = Pipe()
            process.executableURL = URL(fileURLWithPath: AppState.ferroBin)
            process.arguments = ["creds", siteName]
            process.standardOutput = pipe
            guard (try? process.run()) != nil else { return }
            process.waitUntilExit()
            let text = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            for line in text.split(separator: "\n") where line.hasPrefix("password:") {
                let value = line.dropFirst("password:".count)
                    .trimmingCharacters(in: .whitespaces)
                await MainActor.run { password = value }
            }
        }
    }

    private var effectiveLocation: String {
        location.isEmpty ? state.defaultSitesDir : location
    }

    private var arguments: [String] {
        var args = ["new", name, "--php", php, "--wp", wp, "--admin-user", adminUser]
        if !title.isEmpty { args += ["--title", title] }
        if !adminEmail.isEmpty { args += ["--admin-email", adminEmail] }
        if multisite { args += ["--multisite", "subdirectory"] }
        if !location.isEmpty { args += ["--path", "\(location)/\(name)"] }
        if !blueprint.isEmpty { args += ["--blueprint", blueprint] }
        return args
    }

    private func pickLocation() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        if panel.runModal() == .OK, let url = panel.url {
            location = url.path
        }
    }

    private func pickBlueprint() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.init(filenameExtension: "toml")].compactMap { $0 }
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            blueprint = url.path
        }
    }
}

// MARK: - Add Existing Site

/// Adopt a WordPress folder that already exists on disk (exported from
/// Local, MAMP, a backup, or a previous ferro install): pick the folder,
/// optionally a SQL dump, and `ferro adopt` wires it up in place.
struct AddExistingSiteView: View {
    @EnvironmentObject var state: AppState
    @StateObject private var job = FerroProcess()

    @State private var path = ""
    @State private var name = ""
    @State private var php = ""
    @State private var sql = ""
    @State private var fromURL = ""

    private var validName: Bool {
        !name.isEmpty
            && name.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") }
            && name == name.lowercased()
            && !name.hasPrefix("-") && !name.hasSuffix("-")
    }

    private var looksLikeWordPress: Bool {
        !path.isEmpty
            && FileManager.default.fileExists(atPath: path + "/wp-load.php")
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Form {
                HStack {
                    TextField("Site folder:", text: $path,
                              prompt: Text("the existing WordPress folder"))
                    Button("Choose…") { pickFolder() }
                }
                if !path.isEmpty {
                    Text(looksLikeWordPress
                         ? "WordPress found (wp-load.php present)"
                         : "no wp-load.php here — pick the WordPress root")
                        .font(.caption)
                        .foregroundStyle(looksLikeWordPress ? .green : .red)
                }
                if isTCCProtected(path) {
                    Text("macOS protects this folder from background daemons — the site may not load. Prefer a folder outside Desktop/Documents/Downloads, or grant ferrod Full Disk Access.")
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
                TextField("Name:", text: $name)
                    .onChange(of: name) { name = name.lowercased() }
                Text(validName
                     ? "https://\(name).localhost:\(state.httpsPort)"
                     : "letters, digits and hyphens")
                    .font(.caption)
                    .foregroundStyle(validName ? .green : .secondary)
                Picker("PHP:", selection: $php) {
                    ForEach(state.phpVersions, id: \.self) { Text($0).tag($0) }
                }
                HStack {
                    TextField("Database dump (optional):", text: $sql,
                              prompt: Text("a .sql export of the old site"))
                    Button("Choose…") { pickSQL() }
                }
                TextField("Old site URL (optional):", text: $fromURL,
                          prompt: Text("auto-detected from the database"))
                Text("The folder stays where it is. wp-config.php is pointed at the ferro database; with a dump, URLs are rewritten to the local one.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if job.running || job.exitCode != nil {
                OutputLog(text: job.output)
                    .frame(minHeight: 140)
            }

            if job.succeeded {
                HStack {
                    Button("Open Site") {
                        state.open(Site(name: name, path: path, php_version: php, running: true,
                                        db_name: nil, db_user: nil, db_pass: nil))
                    }
                    Button("Open Admin") {
                        state.openAdmin(Site(name: name, path: path, php_version: php, running: true,
                                             db_name: nil, db_user: nil, db_pass: nil))
                    }
                }
            }

            HStack {
                Spacer()
                if job.running { ProgressView().scaleEffect(0.7) }
                Button(job.succeeded ? "Add Another" : "Add Site") { job.run(arguments) }
                    .disabled(!validName || !looksLikeWordPress || job.running)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding()
        .frame(width: 480)
        .onAppear {
            if php.isEmpty { php = state.phpVersions.last ?? "8.3" }
            state.refresh()
        }
        .onChange(of: job.exitCode) {
            if job.exitCode == 0 { state.refresh() }
        }
    }

    private var arguments: [String] {
        var args = ["adopt", name, path, "--php", php]
        if !sql.isEmpty { args += ["--sql", sql] }
        if !fromURL.isEmpty { args += ["--from", fromURL] }
        return args
    }

    private func isTCCProtected(_ path: String) -> Bool {
        let home = NSHomeDirectory() + "/"
        return ["Desktop", "Documents", "Downloads"].contains {
            path == home + $0 || path.hasPrefix(home + $0 + "/")
        }
    }

    private func pickFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        panel.message = "Choose the existing WordPress folder"
        if panel.runModal() == .OK, let url = panel.url {
            path = url.path
            let folder = url.lastPathComponent.lowercased()
            let sanitized = folder
                .map { ($0.isASCII && ($0.isLetter || $0.isNumber)) ? $0 : "-" }
                .reduce(into: "", { $0.append($1) })
                .split(separator: "-", omittingEmptySubsequences: true)
                .joined(separator: "-")
            if name.isEmpty || !validName { name = sanitized }
        }
    }

    private func pickSQL() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.init(filenameExtension: "sql")].compactMap { $0 }
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            sql = url.path
        }
    }
}

// MARK: - Dependencies

/// Bundled components (PHP, MySQL, WP-CLI, …) with one-click installs
/// through `ferro update` — missing pieces are fetched from the UI.
struct DependenciesView: View {
    @EnvironmentObject var state: AppState
    @StateObject private var job = FerroProcess()
    @State private var items: [AppState.DependencyStatus] = []

    private var missing: [AppState.DependencyStatus] {
        items.filter { !$0.installed }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(items) { item in
                HStack {
                    Image(systemName: item.installed
                          ? "checkmark.circle.fill" : "exclamationmark.circle")
                        .foregroundStyle(item.installed ? .green : .orange)
                    VStack(alignment: .leading) {
                        Text(item.title)
                        Text(item.detail)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    if !item.installed {
                        Button("Install") { job.run(["update", item.component]) }
                            .disabled(job.running)
                    }
                }
            }

            if job.running || job.exitCode != nil {
                OutputLog(text: job.output)
                    .frame(minHeight: 140)
            }

            HStack {
                if !missing.isEmpty {
                    Button("Install All Missing (\(missing.count))") {
                        job.run(["update"] + missing.map(\.component))
                    }
                    .disabled(job.running)
                    .keyboardShortcut(.defaultAction)
                }
                Spacer()
                if job.running { ProgressView().scaleEffect(0.7) }
                Button("Refresh") { reload() }
                    .disabled(job.running)
            }
        }
        .padding()
        .frame(width: 480)
        .onAppear { reload() }
        .onChange(of: job.exitCode) {
            reload()
            state.refresh()
        }
    }

    private func reload() {
        items = state.dependencyStatuses()
    }
}

// MARK: - Pull Site

struct PullSiteView: View {
    @EnvironmentObject var state: AppState
    @StateObject private var job = FerroProcess()

    @State private var target = ""
    @State private var remotePath = ""
    @State private var name = ""
    @State private var mediaProxy = true
    @State private var includeUploads = false
    @State private var cacheMedia = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Form {
                TextField("SSH target:", text: $target, prompt: Text("deploy@prod.example.com or ssh alias"))
                TextField("Remote path:", text: $remotePath, prompt: Text("/var/www/html"))
                TextField("Local name (optional):", text: $name)
                Toggle("Media proxy (serve missing uploads from production)", isOn: $mediaProxy)
                if mediaProxy {
                    Toggle("Cache proxied media locally on first hit", isOn: $cacheMedia)
                }
                Toggle("Include uploads in the pull (no proxy needed)", isOn: $includeUploads)
            }
            .onChange(of: includeUploads) { if includeUploads { mediaProxy = false } }
            .onChange(of: mediaProxy) { if mediaProxy { includeUploads = false } }

            if job.running || job.exitCode != nil {
                OutputLog(text: job.output)
                    .frame(minHeight: 140)
            }

            HStack {
                Spacer()
                if job.running { ProgressView().scaleEffect(0.7) }
                Button("Pull") { job.run(arguments) }
                    .disabled(target.isEmpty || remotePath.isEmpty || job.running)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding()
        .frame(width: 460)
        .onAppear { state.refresh() }
    }

    private var arguments: [String] {
        var args = ["pull", target, remotePath]
        if !name.isEmpty { args += ["--name", name.lowercased()] }
        if !mediaProxy { args += ["--no-media-proxy"] }
        if includeUploads { args += ["--include-uploads"] }
        if cacheMedia { args += ["--cache-media"] }
        return args
    }
}

// MARK: - Doctor

struct DoctorView: View {
    @StateObject private var job = FerroProcess()

    var body: some View {
        VStack(spacing: 12) {
            OutputLog(text: job.running ? job.output + "…" : job.output)
            HStack {
                Spacer()
                if job.running { ProgressView().scaleEffect(0.7) }
                Button("Run Again") { job.run(["doctor"]) }
                    .disabled(job.running)
            }
        }
        .padding()
        .frame(width: 560, height: 420)
        .onAppear { job.run(["doctor"]) }
    }
}

// MARK: - Settings

struct SettingsView: View {
    @EnvironmentObject var state: AppState
    @State private var launchAtLogin = false
    @State private var defaultPHP = ""
    @State private var parkedPaths = ""
    @State private var saved = false

    private var configPath: String { AppState.appData + "/config.toml" }

    var body: some View {
        Form {
            Toggle("Launch Ferro at login", isOn: $launchAtLogin)
                .onChange(of: launchAtLogin) { setLaunchAtLogin($0) }

            Picker("Default PHP for new sites:", selection: $defaultPHP) {
                ForEach(state.phpVersions, id: \.self) { Text($0).tag($0) }
            }

            TextField("Parked folders (comma-separated):", text: $parkedPaths)

            Button("Save") { save() }
            if saved {
                Text("Saved — takes effect for new sites immediately.")
                    .font(.caption)
                    .foregroundStyle(.green)
            }
        }
        .formStyle(.grouped)
        .padding()
        .frame(width: 420)
        .onAppear(perform: load)
    }

    private func load() {
        launchAtLogin = SMAppService.mainApp.status == .enabled
        defaultPHP = state.phpVersions.last ?? "8.3"
        guard let text = try? String(contentsOfFile: configPath) else { return }
        var paths: [String] = []
        for line in text.split(separator: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("default_php"),
               let value = trimmed.split(separator: "=").last {
                defaultPHP = value.trimmingCharacters(in: .whitespaces)
                    .trimmingCharacters(in: CharacterSet(charactersIn: "\""))
            }
            if trimmed.hasPrefix("parked_paths"), let value = trimmed.split(separator: "=").last {
                paths = value
                    .replacingOccurrences(of: "[", with: "")
                    .replacingOccurrences(of: "]", with: "")
                    .split(separator: ",")
                    .map { $0.trimmingCharacters(in: .whitespaces)
                        .trimmingCharacters(in: CharacterSet(charactersIn: "\"")) }
            }
        }
        parkedPaths = paths.joined(separator: ", ")
    }

    private func save() {
        guard var text = try? String(contentsOfFile: configPath) else { return }
        text = replaceToml(text, key: "default_php", value: "\"\(defaultPHP)\"")
        let paths = parkedPaths
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .map { "\"\($0)\"" }
            .joined(separator: ", ")
        text = replaceToml(text, key: "parked_paths", value: "[\(paths)]")
        try? text.write(toFile: configPath, atomically: true, encoding: .utf8)
        saved = true
    }

    private func replaceToml(_ text: String, key: String, value: String) -> String {
        var lines = text.split(separator: "\n", omittingEmptySubsequences: false)
            .map(String.init)
        var found = false
        for (index, line) in lines.enumerated() {
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("\(key) ")
                || line.trimmingCharacters(in: .whitespaces).hasPrefix("\(key)=")
                || line.trimmingCharacters(in: .whitespaces).hasPrefix("\(key) =") {
                lines[index] = "\(key) = \(value)"
                found = true
            }
        }
        if !found { lines.append("\(key) = \(value)") }
        return lines.joined(separator: "\n")
    }

    private func setLaunchAtLogin(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
        } catch {
            launchAtLogin = SMAppService.mainApp.status == .enabled
        }
    }
}
