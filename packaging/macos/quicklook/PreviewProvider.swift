// Quick Look on macOS: Space on a file in Finder shows the page Isolinear builds for it (the
// Overview of a standard, the first rows of a table, a document, the text). The page comes from
// the Rust library linked into this extension; it has no script and loads nothing.
import Foundation
import QuickLookUI
import UniformTypeIdentifiers

final class PreviewProvider: QLPreviewProvider, QLPreviewingController {
    func providePreview(for request: QLFilePreviewRequest) async throws -> QLPreviewReply {
        let html = request.fileURL.path.withCString { path -> String in
            guard let page = isolinear_preview_html(path) else {
                return "<!doctype html><p>Isolinear could not read this file.</p>"
            }
            defer { isolinear_preview_free(page) }
            return String(cString: page)
        }
        let data = Data(html.utf8)
        return QLPreviewReply(dataOfContentType: .html, contentSize: CGSize(width: 960, height: 720)) { _ in
            data
        }
    }
}
