import SwiftUI

/// The notices of the open-source code built into the app.
struct LicensesView: View {
    private static let notices: [(name: String, license: String, file: String)] = [
        ("libvpx", "BSD-3-Clause", "libvpx"),
    ]

    var body: some View {
        List(Self.notices, id: \.name) { notice in
            Section("\(notice.name) (\(notice.license))") {
                Text(Self.text(of: notice.file))
                    .font(.footnote.monospaced())
                    .textSelection(.enabled)
            }
        }
        .navigationTitle("Licenses")
    }

    private static func text(of file: String) -> String {
        guard let url = Bundle.main.url(forResource: file, withExtension: "txt"),
              let text = try? String(contentsOf: url, encoding: .utf8) else {
            return "The license text is missing from this build."
        }
        return text
    }
}
