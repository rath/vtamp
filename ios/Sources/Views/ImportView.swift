import SwiftUI

/// Recent YouTube imports on the server, polled while any job is live.
@MainActor
@Observable
final class ImportsModel {
    private(set) var jobs: [ImportJob] = []
    private(set) var error: String?
    private var seen: [String: ImportJob] = [:]

    /// Poll until the task is cancelled: every second while a job is live,
    /// otherwise every ten seconds. Finished or growing jobs refresh the Library.
    /// A job started here is live, so the next one-second tick picks it up.
    func poll(client: VtampClient, app: AppModel) async {
        var last = ContinuousClock.now - .seconds(60)
        while !Task.isCancelled {
            let live = jobs.contains { !$0.isTerminal }
            if live || ContinuousClock.now - last >= .seconds(10) {
                await refresh(client: client, app: app)
                last = .now
            }
            try? await Task.sleep(for: .seconds(1))
        }
    }

    func refresh(client: VtampClient, app: AppModel) async {
        do {
            let jobs = try await client.rpc(.imports, as: [ImportJob].self)
            var changed = false
            for job in jobs {
                if let before = seen[job.jobId] {
                    if job.added + job.updated > before.added + before.updated || (job.isTerminal && !before.isTerminal) {
                        changed = true
                    }
                }
                seen[job.jobId] = job
            }
            self.jobs = jobs
            error = nil
            app.reachable()
            if changed { app.libraryChanged() }
        } catch {
            self.error = error.localizedDescription
            app.report(error)
        }
    }

    func start(_ request: ImportRequest, client: VtampClient, app: AppModel) async throws(VtampError) {
        _ = try await client.rpc(.importStart(request), as: ImportStarted.self)
        await refresh(client: client, app: app)
    }

    func cancel(_ job: ImportJob, client: VtampClient, app: AppModel) async {
        do {
            _ = try await client.rpc(.importCancel(id: job.jobId), as: ImportJob.self)
        } catch {
            self.error = error.localizedDescription
        }
        await refresh(client: client, app: app)
    }

    func retry(_ job: ImportJob, client: VtampClient, app: AppModel) async {
        do {
            _ = try await client.rpc(.importRetry(id: job.jobId), as: ImportStarted.self)
        } catch {
            self.error = error.localizedDescription
        }
        await refresh(client: client, app: app)
    }
}

struct ImportView: View {
    @Environment(AppModel.self) private var app
    @Environment(ImportsModel.self) private var imports
    @Environment(Player.self) private var player
    @State private var url = ""
    @State private var wholePlaylist = false
    @State private var start = ""
    @State private var end = ""
    @State private var submitting = false
    @State private var message: String?
    @FocusState private var editing: Bool

    private var namesPlaylist: Bool {
        URLComponents(string: url.trimmingCharacters(in: .whitespaces))?
            .queryItems?.contains { $0.name == "list" } ?? false
    }

    private var range: Result<TimeRange?, TimeParseError> {
        Result { () throws(TimeParseError) in try TimeParsing.range(start: start, end: end) }
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("https://www.youtube.com/watch?v=…", text: $url)
                        .focused($editing)
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    PasteButton(payloadType: String.self) { strings in
                        if let first = strings.first { url = first.trimmingCharacters(in: .whitespacesAndNewlines) }
                    }
                    if namesPlaylist {
                        Toggle("Import the whole playlist", isOn: $wholePlaylist)
                    }
                } header: {
                    Text("YouTube URL")
                } footer: {
                    Text("The server downloads the audio with yt-dlp and adds it to the Library.")
                }
                if !(namesPlaylist && wholePlaylist) {
                    Section {
                        TextField("Start (0:00)", text: $start)
                            .focused($editing)
                            .keyboardType(.numbersAndPunctuation)
                        TextField("End (end of video)", text: $end)
                            .focused($editing)
                            .keyboardType(.numbersAndPunctuation)
                    } header: {
                        Text("Time range")
                    } footer: {
                        if case let .failure(error) = range {
                            Text(error.localizedDescription).foregroundStyle(.red)
                        } else {
                            Text("Optional. Seconds, M:SS, or H:MM:SS.")
                        }
                    }
                }
                Section {
                    Button {
                        Task { await submit() }
                    } label: {
                        HStack {
                            Text("Import")
                            if submitting { Spacer(); ProgressView() }
                        }
                    }
                    .disabled(!canSubmit)
                    if let message {
                        Text(message).font(.footnote).foregroundStyle(.secondary)
                    }
                }
                if !imports.jobs.isEmpty {
                    Section("Recent imports") {
                        ForEach(imports.jobs) { job in
                            ImportJobRow(job: job, play: play, cancel: cancel, retry: retry)
                        }
                    }
                }
                if let error = imports.error {
                    Section { Text(error).font(.footnote).foregroundStyle(.red) }
                }
            }
            .scrollDismissesKeyboard(.interactively)
            .navigationTitle("Import")
        }
    }

    private var canSubmit: Bool {
        guard !submitting, app.client != nil, !url.trimmingCharacters(in: .whitespaces).isEmpty else { return false }
        if namesPlaylist && wholePlaylist { return true }
        if case .failure = range { return false }
        return true
    }

    private func submit() async {
        guard let client = app.client else { return }
        editing = false
        let playlist = namesPlaylist && wholePlaylist
        var request = ImportRequest(url: url.trimmingCharacters(in: .whitespacesAndNewlines), playlist: playlist)
        if !playlist, case let .success(range) = range {
            request.range = range
        }
        submitting = true
        defer { submitting = false }
        do {
            // The new job appears under Recent imports with its progress.
            try await imports.start(request, client: client, app: app)
            message = nil
            url = ""
            start = ""
            end = ""
            wholePlaylist = false
        } catch {
            message = error.localizedDescription
        }
    }

    private func play(_ job: ImportJob) {
        guard let client = app.client, let id = job.firstAddedTrackId else { return }
        Task {
            do {
                player.playNow(try await client.rpc(.libraryTrack(id: id), as: Track.self))
            } catch {
                message = error.localizedDescription
            }
        }
    }

    private func cancel(_ job: ImportJob) {
        guard let client = app.client else { return }
        Task { await imports.cancel(job, client: client, app: app) }
    }

    private func retry(_ job: ImportJob) {
        guard let client = app.client else { return }
        Task { await imports.retry(job, client: client, app: app) }
    }
}

struct ImportJobRow: View {
    let job: ImportJob
    let play: (ImportJob) -> Void
    let cancel: (ImportJob) -> Void
    let retry: (ImportJob) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(job.title).lineLimit(2)
            HStack(spacing: 6) {
                Text(job.isTerminal ? job.status.capitalized : job.stageText)
                if let range = job.range { Text("· \(range.label)") }
                if let total = job.total, total > 1 {
                    Text("· \(job.added + job.updated + job.skipped + job.failed) of \(total)")
                }
            }
            .font(.footnote)
            .foregroundStyle(.secondary)
            if !job.isTerminal {
                if let fraction = job.fraction {
                    ProgressView(value: fraction)
                } else {
                    ProgressView().frame(maxWidth: .infinity, alignment: .leading)
                }
                if let detail { Text(detail).font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
            }
            if let error = job.error {
                Text(error).font(.footnote).foregroundStyle(.red).lineLimit(3)
            }
            HStack {
                if job.firstAddedTrackId != nil {
                    Button("Play", systemImage: "play.fill") { play(job) }
                }
                if !job.isTerminal {
                    Button("Cancel", systemImage: "xmark", role: .destructive) { cancel(job) }
                } else if job.isRetryable {
                    Button("Retry", systemImage: "arrow.clockwise") { retry(job) }
                }
            }
            .buttonStyle(.borderless)
            .font(.footnote)
        }
        .padding(.vertical, 2)
    }

    private var detail: String? {
        let progress = job.progress
        var parts: [String] = []
        if let processed = progress.processedMs, let total = progress.processingTotalMs {
            parts.append("\(Formatting.time(milliseconds: processed)) of \(Formatting.time(milliseconds: total))")
            if let speed = progress.processingSpeed { parts.append(String(format: "%.1f×", speed)) }
        } else if let bytes = progress.bytes {
            parts.append(progress.total.map { "\(Formatting.bytes(bytes)) of \(Formatting.bytes($0))" } ?? Formatting.bytes(bytes))
            if let speed = progress.speed { parts.append("\(Formatting.bytes(Int64(speed)))/s") }
        }
        if let eta = progress.eta { parts.append("\(Formatting.time(interval: eta)) left") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}
