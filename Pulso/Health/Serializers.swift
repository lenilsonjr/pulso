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
            metadata: recordedZone.map { ["timeZone": $0.identifier] }
        )
    }

    static func quantity(
        type: String, unit: HKUnit, unitLabel: String
    ) -> @Sendable (HKSample, SerializerContext) -> SampleDTO? {
        { sample, context in
            guard let quantitySample = sample as? HKQuantitySample else { return nil }
            var dto = base(quantitySample, type: type, context: context)
            dto.value = .number(quantitySample.quantity.doubleValue(for: unit))
            dto.unit = unitLabel
            return dto
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

    @Sendable
    static func workout(_ sample: HKSample, context: SerializerContext) -> SampleDTO? {
        guard let workout = sample as? HKWorkout else { return nil }
        var dto = base(workout, type: "workout", context: context)
        dto.workoutActivityType = WorkoutActivityName.name(for: workout.workoutActivityType)
        dto.duration = workout.duration
        dto.totalEnergyBurned = workout.statistics(for: HKQuantityType(.activeEnergyBurned))?
            .sumQuantity()?.doubleValue(for: .kilocalorie())
        if dto.totalEnergyBurned == nil {
            // Workouts recorded before iOS 16 carry totals only in the legacy property.
            dto.totalEnergyBurned = workout.totalEnergyBurned?.doubleValue(for: .kilocalorie())
        }
        dto.totalDistance = distanceMeters(of: workout)
        return dto
    }

    private static let distanceTypes: [HKQuantityTypeIdentifier] = [
        .distanceWalkingRunning, .distanceCycling, .distanceSwimming,
        .distanceWheelchair, .distanceDownhillSnowSports,
    ]

    private static func distanceMeters(of workout: HKWorkout) -> Double? {
        for identifier in distanceTypes {
            if let sum = workout.statistics(for: HKQuantityType(identifier))?.sumQuantity() {
                return sum.doubleValue(for: .meter())
            }
        }
        return workout.totalDistance?.doubleValue(for: .meter())
    }
}
