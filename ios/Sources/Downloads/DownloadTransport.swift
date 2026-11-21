import Foundation

/// Fetches one URL into a file, so the downloads logic is testable without a
/// server.
protocol DownloadTransport: Sendable {
    /// The file belongs to the caller, who moves or removes it. `progress`
    /// gets the fraction received, from 0 to 1, when the server names a length.
    func fetch(_ url: URL, progress: @escaping @Sendable (Double) -> Void) async throws -> URL
}

enum DownloadError: LocalizedError, Equatable {
    case http(Int)
    case incomplete

    var errorDescription: String? {
        switch self {
        case let .http(status): "The server answered HTTP \(status)"
        case .incomplete: "The download did not finish"
        }
    }
}

/// A download task per file. The transfer runs while the app does: in the
/// foreground, or in the background while it plays.
final class URLSessionDownloadTransport: DownloadTransport {
    private let delegate = Delegate()
    private let session: URLSession

    init() {
        let configuration = URLSessionConfiguration.default
        configuration.timeoutIntervalForRequest = 30
        // The file is kept on disk; a second copy in the URL cache helps nobody.
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        session = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
    }

    func fetch(_ url: URL, progress: @escaping @Sendable (Double) -> Void) async throws -> URL {
        try Task.checkCancellation()
        let task = session.downloadTask(with: url)
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                delegate.begin(task.taskIdentifier, continuation: continuation, progress: progress)
                task.resume()
            }
        } onCancel: {
            task.cancel()
        }
    }

    private final class Delegate: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
        private struct Transfer {
            let continuation: CheckedContinuation<URL, any Error>
            let progress: @Sendable (Double) -> Void
            var outcome: Result<URL, any Error>?
        }

        private let lock = NSLock()
        private var transfers: [Int: Transfer] = [:]
        /// Tasks cancelled before their caller was waiting.
        private var endedEarly: [Int: any Error] = [:]

        func begin(
            _ id: Int, continuation: CheckedContinuation<URL, any Error>,
            progress: @escaping @Sendable (Double) -> Void
        ) {
            let early: (any Error)? = lock.withLock {
                if let error = endedEarly.removeValue(forKey: id) { return error }
                transfers[id] = Transfer(continuation: continuation, progress: progress)
                return nil
            }
            if let early { continuation.resume(throwing: Self.reported(early)) }
        }

        private static func reported(_ error: any Error) -> any Error {
            (error as? URLError)?.code == .cancelled ? CancellationError() : error
        }

        func urlSession(
            _ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData bytesWritten: Int64,
            totalBytesWritten: Int64, totalBytesExpectedToWrite: Int64
        ) {
            guard totalBytesExpectedToWrite > 0 else { return }
            let progress = lock.withLock { transfers[downloadTask.taskIdentifier]?.progress }
            progress?(Double(totalBytesWritten) / Double(totalBytesExpectedToWrite))
        }

        /// The system removes `location` when this returns, so the file moves now.
        func urlSession(
            _ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL
        ) {
            let outcome: Result<URL, any Error>
            if let status = (downloadTask.response as? HTTPURLResponse)?.statusCode, status != 200 {
                outcome = .failure(DownloadError.http(status))
            } else {
                let kept = FileManager.default.temporaryDirectory
                    .appending(component: "vtamp-download-\(UUID().uuidString)")
                outcome = Result { try FileManager.default.moveItem(at: location, to: kept) }.map { kept }
            }
            lock.withLock { transfers[downloadTask.taskIdentifier]?.outcome = outcome }
        }

        func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: (any Error)?) {
            let transfer: Transfer? = lock.withLock {
                if let transfer = transfers.removeValue(forKey: task.taskIdentifier) { return transfer }
                if let error { endedEarly[task.taskIdentifier] = error }
                return nil
            }
            guard let transfer else { return }
            if let error {
                // A file that arrived just before a cancellation is not wanted.
                if case let .success(url) = transfer.outcome { try? FileManager.default.removeItem(at: url) }
                transfer.continuation.resume(throwing: Self.reported(error))
            } else {
                transfer.continuation.resume(with: transfer.outcome ?? .failure(DownloadError.incomplete))
            }
        }
    }
}
