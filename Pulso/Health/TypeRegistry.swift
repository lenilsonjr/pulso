import Foundation
import HealthKit

/// One syncable HealthKit type: its wire key, how eagerly HealthKit may wake
/// us for it, and how its samples serialize. Adding a type is a table row.
struct SyncedType: Identifiable {
    let key: String // wire "type" value and persistence key
    let displayName: String
    let sampleType: HKSampleType
    let frequency: HKUpdateFrequency
    let serialize: @Sendable (HKSample, SerializerContext) -> SampleDTO?

    var id: String { key }
}

enum TypeRegistry {
    /// Sleep and workouts are what this app exists for — they get .immediate
    /// background delivery. Everything else is capped at .hourly by HealthKit.
    static let all: [SyncedType] = [
        SyncedType(
            key: "sleepAnalysis", displayName: "Sleep",
            sampleType: HKCategoryType(.sleepAnalysis),
            frequency: .immediate, serialize: Serializers.sleep
        ),
        SyncedType(
            key: "workout", displayName: "Workouts",
            sampleType: HKObjectType.workoutType(),
            frequency: .immediate, serialize: Serializers.workout
        ),
        SyncedType(
            key: "stepCount", displayName: "Steps",
            sampleType: HKQuantityType(.stepCount),
            frequency: .hourly,
            serialize: Serializers.quantity(type: "stepCount", unit: .count(), unitLabel: "count")
        ),
        SyncedType(
            key: "heartRate", displayName: "Heart Rate",
            sampleType: HKQuantityType(.heartRate),
            frequency: .hourly,
            serialize: Serializers.quantity(
                type: "heartRate", unit: HKUnit.count().unitDivided(by: .minute()), unitLabel: "count/min"
            )
        ),
        SyncedType(
            key: "restingHeartRate", displayName: "Resting Heart Rate",
            sampleType: HKQuantityType(.restingHeartRate),
            frequency: .hourly,
            serialize: Serializers.quantity(
                type: "restingHeartRate", unit: HKUnit.count().unitDivided(by: .minute()), unitLabel: "count/min"
            )
        ),
        SyncedType(
            key: "heartRateVariabilitySDNN", displayName: "HRV (SDNN)",
            sampleType: HKQuantityType(.heartRateVariabilitySDNN),
            frequency: .hourly,
            serialize: Serializers.quantity(
                type: "heartRateVariabilitySDNN", unit: .secondUnit(with: .milli), unitLabel: "ms"
            )
        ),
        SyncedType(
            key: "activeEnergyBurned", displayName: "Active Energy",
            sampleType: HKQuantityType(.activeEnergyBurned),
            frequency: .hourly,
            serialize: Serializers.quantity(type: "activeEnergyBurned", unit: .kilocalorie(), unitLabel: "kcal")
        ),
        SyncedType(
            key: "bodyMass", displayName: "Body Mass",
            sampleType: HKQuantityType(.bodyMass),
            frequency: .hourly,
            serialize: Serializers.quantity(type: "bodyMass", unit: .gramUnit(with: .kilo), unitLabel: "kg")
        ),
    ]

    static func type(for key: String) -> SyncedType? {
        all.first { $0.key == key }
    }

    static var readTypes: Set<HKObjectType> {
        Set(all.map { $0.sampleType as HKObjectType })
    }
}
