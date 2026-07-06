import Foundation

/// Wire-format constants and encoders. The upload body is a bare JSON array
/// of batch elements; outbox files add a small envelope around the same
/// elements (see Batch).
enum Wire {
    /// Uncompressed batch ceiling. JSON this size gzips to roughly 2 MB,
    /// the batch budget from the design doc.
    static let maxBatchBytes = 8 * 1024 * 1024

    static func encodeElements(_ elements: [BatchElement]) throws -> Data {
        try bodyEncoder().encode(elements)
    }

    static func bodyEncoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        return encoder
    }

    static func fileEncoder() -> JSONEncoder {
        let encoder = bodyEncoder()
        encoder.dateEncodingStrategy = .iso8601
        return encoder
    }

    static func fileDecoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return decoder
    }
}

enum BatchSlicer {
    /// Splits elements in half recursively until every slice encodes under
    /// maxBytes. A single element is never split, whatever its size.
    static func slice(_ elements: [BatchElement], maxBytes: Int) throws -> [[BatchElement]] {
        guard elements.count > 1 else { return [elements] }
        let encoded = try Wire.encodeElements(elements)
        guard encoded.count > maxBytes else { return [elements] }
        let mid = elements.count / 2
        return try slice(Array(elements[..<mid]), maxBytes: maxBytes)
            + slice(Array(elements[mid...]), maxBytes: maxBytes)
    }
}
