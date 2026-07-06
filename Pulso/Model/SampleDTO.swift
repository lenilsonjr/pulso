import Foundation

/// One health sample as it appears on the wire. Optional fields are omitted
/// when absent. See docs/PROTOCOL.md for the authoritative schema.
struct SampleDTO: Codable, Equatable, Sendable {
    var uuid: String
    var type: String
    var start: String
    var end: String
    var value: SampleValue? = nil
    var unit: String? = nil
    var source: String
    var sourceBundleId: String? = nil
    /// Only carries "timeZone", and only when the sample itself recorded one
    /// (HKMetadataKeyTimeZone). Absent means the timestamps' offset is the
    /// device timezone at sync time — an assumption, not a recorded fact.
    var metadata: [String: String]? = nil

    // Workouts only.
    var workoutActivityType: String? = nil
    var duration: Double? = nil
    var totalEnergyBurned: Double? = nil
    var totalDistance: Double? = nil
}

/// Category samples carry a string (e.g. "asleepREM"); quantity samples a number.
enum SampleValue: Codable, Equatable, Sendable {
    case string(String)
    case number(Double)

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let s = try? container.decode(String.self) {
            self = .string(s)
        } else if let n = try? container.decode(Double.self) {
            self = .number(n)
        } else {
            throw DecodingError.typeMismatch(
                SampleValue.self,
                .init(codingPath: decoder.codingPath, debugDescription: "value must be a string or a number")
            )
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let s): try container.encode(s)
        case .number(let n): try container.encode(n)
        }
    }
}

/// An element of an /ingest batch: either a sample or a deletion tombstone
/// ({"deleted": ["uuid", ...]}) surfaced by an anchored query.
enum BatchElement: Codable, Equatable, Sendable {
    case sample(SampleDTO)
    case tombstone([String])

    private enum TombstoneKeys: String, CodingKey { case deleted }

    var sampleCount: Int {
        if case .sample = self { return 1 }
        return 0
    }

    init(from decoder: Decoder) throws {
        if let container = try? decoder.container(keyedBy: TombstoneKeys.self), container.contains(.deleted) {
            self = .tombstone(try container.decode([String].self, forKey: .deleted))
        } else {
            self = .sample(try SampleDTO(from: decoder))
        }
    }

    func encode(to encoder: Encoder) throws {
        switch self {
        case .sample(let dto):
            try dto.encode(to: encoder)
        case .tombstone(let uuids):
            var container = encoder.container(keyedBy: TombstoneKeys.self)
            try container.encode(uuids, forKey: .deleted)
        }
    }
}

/// One durable outbox file: a slice of query results plus the anchor to
/// persist once the server ACKs it. `anchor` is nil on every slice but the
/// last when a single query result is split by size — the anchor may only
/// advance once *all* of the result is delivered.
struct Batch: Codable, Sendable {
    let id: UUID
    let typeKey: String
    let createdAt: Date
    let anchor: Data?
    let elements: [BatchElement]

    var sampleCount: Int { elements.reduce(0) { $0 + $1.sampleCount } }
}
