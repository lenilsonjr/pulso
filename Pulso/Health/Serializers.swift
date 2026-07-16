import Foundation
import HealthKit

struct SerializerContext: Sendable {
    /// Read at serialize time so long-running backfills track timezone changes.
    let defaultTimeZone: @Sendable () -> TimeZone
    let formatter: TimestampFormatter

    static let live = SerializerContext(defaultTimeZone: { .current }, formatter: TimestampFormatter())
}

enum Serializers {
    /// Fields shared by every sample type. Timestamps keep the sample's own
    /// timezone when it recorded one (HKMetadataKeyTimeZone); otherwise the
    /// device timezone is used and `metadata.timeZone` stays absent so
    /// consumers can tell a recorded timezone from an assumed one.
    static func base(_ sample: HKSample, type: String, context: SerializerContext) -> SampleDTO {
        let recordedZone = (sample.metadata?[HKMetadataKeyTimeZone] as? String).flatMap {
            TimeZone(identifier: $0) ?? TimeZone(abbreviation: $0)
        }
        let zone = recordedZone ?? context.defaultTimeZone()
        return SampleDTO(
            uuid: sample.uuid.uuidString,
            type: type,
            start: context.formatter.string(sample.startDate, in: zone),
            end: context.formatter.string(sample.endDate, in: zone),
            source: sample.sourceRevision.source.name,
            sourceBundleId: sample.sourceRevision.source.bundleIdentifier,
            metadata: recordedZone.map { ["timeZone": .string($0.identifier)] }
        )
    }

    /// `scale` multiplies the converted value before it goes on the wire.
    /// Used for HealthKit's percent types, which read as fractions (0–1)
    /// via HKUnit.percent(): scale 100 delivers human-scale 0–100 under a
    /// "%" unit label.
    static func quantity(
        type: String, unit: HKUnit, unitLabel: String, scale: Double = 1
    ) -> @Sendable (HKSample, SerializerContext) -> SampleDTO? {
        { sample, context in
            guard let quantitySample = sample as? HKQuantitySample else { return nil }
            var dto = base(quantitySample, type: type, context: context)
            dto.value = .number(quantitySample.quantity.doubleValue(for: unit) * scale)
            dto.unit = unitLabel
            return dto
        }
    }

    /// Generic category-sample serializer. `valueName` maps the raw category
    /// value to its wire name; returning nil omits `value` entirely (for
    /// types like mindfulSession whose value is notApplicable — the interval
    /// itself is the data).
    static func category(
        type: String, valueName: @escaping @Sendable (Int) -> String?
    ) -> @Sendable (HKSample, SerializerContext) -> SampleDTO? {
        { sample, context in
            guard let categorySample = sample as? HKCategorySample else { return nil }
            var dto = base(categorySample, type: type, context: context)
            dto.value = valueName(categorySample.value).map(SampleValue.string)
            return dto
        }
    }

    static func standHourName(_ rawValue: Int) -> String {
        switch HKCategoryValueAppleStandHour(rawValue: rawValue) {
        case .stood: "stood"
        case .idle: "idle"
        default: "value_\(rawValue)"
        }
    }

    @Sendable
    static func sleep(_ sample: HKSample, context: SerializerContext) -> SampleDTO? {
        guard let categorySample = sample as? HKCategorySample else { return nil }
        var dto = base(categorySample, type: "sleepAnalysis", context: context)
        dto.value = .string(sleepStageName(categorySample.value))
        return dto
    }

    static func sleepStageName(_ rawValue: Int) -> String {
        switch HKCategoryValueSleepAnalysis(rawValue: rawValue) {
        case .inBed: "inBed"
        case .asleepUnspecified: "asleepUnspecified"
        case .awake: "awake"
        case .asleepCore: "asleepCore"
        case .asleepDeep: "asleepDeep"
        case .asleepREM: "asleepREM"
        default: "value_\(rawValue)"
        }
    }

    // MARK: - State of Mind (iOS 18+)

    /// HKStateOfMind is its own HKSample subclass (neither category nor
    /// quantity): a valence in [-1, +1] plus a kind, discrete emotion labels,
    /// and life-area associations. Valence rides `value`; the enums ride
    /// metadata as comma-joined names so the wire format stays scalar.
    @available(iOS 18.0, *)
    @Sendable
    static func stateOfMind(_ sample: HKSample, context: SerializerContext) -> SampleDTO? {
        guard let mood = sample as? HKStateOfMind else { return nil }
        var dto = base(mood, type: "stateOfMind", context: context)
        dto.value = .number(mood.valence)
        dto.unit = "valence"
        var metadata = dto.metadata ?? [:]
        metadata["kind"] = .string(stateOfMindKindName(mood.kind))
        metadata["valenceClassification"] = .string(valenceClassificationName(mood.valenceClassification))
        if !mood.labels.isEmpty {
            metadata["labels"] = .string(mood.labels.map(stateOfMindLabelName).joined(separator: ","))
        }
        if !mood.associations.isEmpty {
            metadata["associations"] = .string(
                mood.associations.map(stateOfMindAssociationName).joined(separator: ","))
        }
        dto.metadata = metadata
        return dto
    }

    @available(iOS 18.0, *)
    static func stateOfMindKindName(_ kind: HKStateOfMind.Kind) -> String {
        switch kind {
        case .momentaryEmotion: "momentaryEmotion"
        case .dailyMood: "dailyMood"
        @unknown default: "kind_\(kind.rawValue)"
        }
    }

    @available(iOS 18.0, *)
    static func valenceClassificationName(_ c: HKStateOfMind.ValenceClassification) -> String {
        switch c {
        case .veryUnpleasant: "veryUnpleasant"
        case .unpleasant: "unpleasant"
        case .slightlyUnpleasant: "slightlyUnpleasant"
        case .neutral: "neutral"
        case .slightlyPleasant: "slightlyPleasant"
        case .pleasant: "pleasant"
        case .veryPleasant: "veryPleasant"
        @unknown default: "classification_\(c.rawValue)"
        }
    }

    @available(iOS 18.0, *)
    static func stateOfMindLabelName(_ label: HKStateOfMind.Label) -> String {
        switch label {
        case .amazed: "amazed"
        case .amused: "amused"
        case .angry: "angry"
        case .annoyed: "annoyed"
        case .anxious: "anxious"
        case .ashamed: "ashamed"
        case .brave: "brave"
        case .calm: "calm"
        case .confident: "confident"
        case .content: "content"
        case .disappointed: "disappointed"
        case .discouraged: "discouraged"
        case .disgusted: "disgusted"
        case .drained: "drained"
        case .embarrassed: "embarrassed"
        case .excited: "excited"
        case .frustrated: "frustrated"
        case .grateful: "grateful"
        case .guilty: "guilty"
        case .happy: "happy"
        case .hopeful: "hopeful"
        case .hopeless: "hopeless"
        case .indifferent: "indifferent"
        case .irritated: "irritated"
        case .jealous: "jealous"
        case .joyful: "joyful"
        case .lonely: "lonely"
        case .overwhelmed: "overwhelmed"
        case .passionate: "passionate"
        case .peaceful: "peaceful"
        case .proud: "proud"
        case .relieved: "relieved"
        case .sad: "sad"
        case .satisfied: "satisfied"
        case .scared: "scared"
        case .stressed: "stressed"
        case .surprised: "surprised"
        case .worried: "worried"
        @unknown default: "label_\(label.rawValue)"
        }
    }

    @available(iOS 18.0, *)
    static func stateOfMindAssociationName(_ a: HKStateOfMind.Association) -> String {
        switch a {
        case .community: "community"
        case .currentEvents: "currentEvents"
        case .dating: "dating"
        case .education: "education"
        case .family: "family"
        case .fitness: "fitness"
        case .friends: "friends"
        case .health: "health"
        case .hobbies: "hobbies"
        case .identity: "identity"
        case .money: "money"
        case .partner: "partner"
        case .selfCare: "selfCare"
        case .spirituality: "spirituality"
        case .tasks: "tasks"
        case .travel: "travel"
        case .weather: "weather"
        case .work: "work"
        @unknown default: "association_\(a.rawValue)"
        }
    }

    @Sendable
    static func workout(_ sample: HKSample, context: SerializerContext) -> SampleDTO? {
        guard let workout = sample as? HKWorkout else { return nil }
        var dto = base(workout, type: "workout", context: context)
        dto.workoutActivityType = WorkoutActivityName.name(for: workout.workoutActivityType)
        dto.duration = workout.duration
        // Statistics first; legacy totals (pre-iOS-16 recordings) as fallback.
        // HealthKit synthesizes zero-valued statistics for legacy-total
        // workouts, so zero sums count as absent — otherwise they shadow the
        // real legacy value.
        dto.totalEnergyBurned = positiveSum(workout, .activeEnergyBurned, unit: .kilocalorie())
            ?? workout.totalEnergyBurned?.doubleValue(for: .kilocalorie())
        dto.totalBasalEnergyBurned = positiveSum(workout, .basalEnergyBurned, unit: .kilocalorie())
        dto.totalDistance = distanceMeters(of: workout)

        let bpm = HKUnit.count().unitDivided(by: .minute())
        if let heartRate = workout.statistics(for: HKQuantityType(.heartRate)) {
            dto.averageHeartRate = heartRate.averageQuantity()?.doubleValue(for: bpm)
            dto.minimumHeartRate = heartRate.minimumQuantity()?.doubleValue(for: bpm)
            dto.maximumHeartRate = heartRate.maximumQuantity()?.doubleValue(for: bpm)
        }

        // Workouts carry their full metadata (routine names from third-party
        // apps, indoor flag, weather, …). HKTimeZone is folded into the
        // protocol's "timeZone" key by base() rather than passed through raw.
        var metadata = dto.metadata ?? [:]
        for (key, value) in workout.metadata ?? [:] where key != HKMetadataKeyTimeZone {
            metadata[key] = jsonValue(value, context: context)
        }
        dto.metadata = metadata.isEmpty ? nil : metadata
        return dto
    }

    /// Strings, numbers, and booleans pass through natively; HKQuantity,
    /// dates, and anything else are stringified.
    static func jsonValue(_ value: Any, context: SerializerContext) -> JSONValue {
        switch value {
        case let string as String:
            return .string(string)
        case let number as NSNumber:
            if CFGetTypeID(number) == CFBooleanGetTypeID() {
                return .bool(number.boolValue)
            }
            return .number(number.doubleValue)
        case let date as Date:
            return .string(context.formatter.string(date, in: context.defaultTimeZone()))
        default:
            return .string(String(describing: value))
        }
    }

    private static let distanceTypes: [HKQuantityTypeIdentifier] = [
        .distanceWalkingRunning, .distanceCycling, .distanceSwimming,
        .distanceWheelchair, .distanceDownhillSnowSports,
    ]

    private static func distanceMeters(of workout: HKWorkout) -> Double? {
        for identifier in distanceTypes {
            if let meters = positiveSum(workout, identifier, unit: .meter()) {
                return meters
            }
        }
        return workout.totalDistance?.doubleValue(for: .meter())
    }

    private static func positiveSum(
        _ workout: HKWorkout, _ identifier: HKQuantityTypeIdentifier, unit: HKUnit
    ) -> Double? {
        guard let sum = workout.statistics(for: HKQuantityType(identifier))?.sumQuantity() else { return nil }
        let value = sum.doubleValue(for: unit)
        return value > 0 ? value : nil
    }
}
