import HealthKit
import XCTest
@testable import Pulso

final class SerializerTests: XCTestCase {
    /// Fixed non-DST zone so the fallback path is deterministic.
    private let context = SerializerContext(
        defaultTimeZone: { TimeZone(identifier: "Asia/Dubai")! },
        formatter: TimestampFormatter()
    )

    func testSleepSampleKeepsItsRecordedTimeZone() throws {
        let start = TestSupport.date(2026, 7, 6, 1, 12, zone: "Europe/Lisbon")
        let end = TestSupport.date(2026, 7, 6, 2, 40, zone: "Europe/Lisbon")
        let sample = HKCategorySample(
            type: HKCategoryType(.sleepAnalysis),
            value: HKCategoryValueSleepAnalysis.asleepREM.rawValue,
            start: start,
            end: end,
            metadata: [HKMetadataKeyTimeZone: "Europe/Lisbon"]
        )

        let dto = try XCTUnwrap(Serializers.sleep(sample, context: context))
        XCTAssertEqual(dto.type, "sleepAnalysis")
        XCTAssertEqual(dto.value, .string("asleepREM"))
        XCTAssertEqual(dto.start, "2026-07-06T01:12:00+01:00")
        XCTAssertEqual(dto.end, "2026-07-06T02:40:00+01:00")
        XCTAssertEqual(dto.metadata, ["timeZone": "Europe/Lisbon"])
        XCTAssertEqual(dto.uuid, sample.uuid.uuidString)
        XCTAssertNil(dto.unit)
    }

    func testQuantitySampleFallsBackToDeviceZoneWithoutRecordedOne() throws {
        let start = TestSupport.date(2026, 7, 6, 8, 0, zone: "Asia/Dubai")
        let unit = HKUnit.count().unitDivided(by: .minute())
        let sample = HKQuantitySample(
            type: HKQuantityType(.heartRate),
            quantity: HKQuantity(unit: unit, doubleValue: 58),
            start: start,
            end: start
        )

        let serialize = Serializers.quantity(type: "heartRate", unit: unit, unitLabel: "count/min")
        let dto = try XCTUnwrap(serialize(sample, context))
        XCTAssertEqual(dto.type, "heartRate")
        XCTAssertEqual(dto.value, .number(58))
        XCTAssertEqual(dto.unit, "count/min")
        XCTAssertEqual(dto.start, "2026-07-06T08:00:00+04:00")
        XCTAssertNil(dto.metadata, "assumed timezone must not masquerade as a recorded one")
    }

    func testQuantitySerializerRejectsWrongSampleClass() {
        let sample = HKCategorySample(
            type: HKCategoryType(.sleepAnalysis),
            value: HKCategoryValueSleepAnalysis.awake.rawValue,
            start: Date(),
            end: Date()
        )
        let serialize = Serializers.quantity(type: "stepCount", unit: .count(), unitLabel: "count")
        XCTAssertNil(serialize(sample, context))
    }

    func testWorkoutSerialization() throws {
        let start = TestSupport.date(2026, 7, 5, 18, 30, zone: "Asia/Dubai")
        let end = start.addingTimeInterval(3612)
        let workout = HKWorkout(activityType: .traditionalStrengthTraining, start: start, end: end)

        let dto = try XCTUnwrap(Serializers.workout(workout, context: context))
        XCTAssertEqual(dto.type, "workout")
        XCTAssertEqual(dto.workoutActivityType, "traditionalStrengthTraining")
        XCTAssertEqual(dto.duration ?? 0, 3612, accuracy: 0.5)
        XCTAssertEqual(dto.start, "2026-07-05T18:30:00+04:00")
        XCTAssertNil(dto.value)
        // Synthetic workout carries no statistics; fields stay absent rather than 0.
        XCTAssertNil(dto.totalEnergyBurned)
        XCTAssertNil(dto.totalDistance)
    }

    func testSleepStageNames() {
        XCTAssertEqual(Serializers.sleepStageName(HKCategoryValueSleepAnalysis.inBed.rawValue), "inBed")
        XCTAssertEqual(Serializers.sleepStageName(HKCategoryValueSleepAnalysis.asleepUnspecified.rawValue), "asleepUnspecified")
        XCTAssertEqual(Serializers.sleepStageName(HKCategoryValueSleepAnalysis.awake.rawValue), "awake")
        XCTAssertEqual(Serializers.sleepStageName(HKCategoryValueSleepAnalysis.asleepCore.rawValue), "asleepCore")
        XCTAssertEqual(Serializers.sleepStageName(HKCategoryValueSleepAnalysis.asleepDeep.rawValue), "asleepDeep")
        XCTAssertEqual(Serializers.sleepStageName(HKCategoryValueSleepAnalysis.asleepREM.rawValue), "asleepREM")
        XCTAssertEqual(Serializers.sleepStageName(9999), "value_9999")
    }

    func testWorkoutActivityNames() {
        XCTAssertEqual(WorkoutActivityName.name(for: .running), "running")
        XCTAssertEqual(WorkoutActivityName.name(for: .traditionalStrengthTraining), "traditionalStrengthTraining")
        XCTAssertEqual(WorkoutActivityName.name(for: .highIntensityIntervalTraining), "highIntensityIntervalTraining")
        XCTAssertEqual(WorkoutActivityName.name(for: .other), "other")
    }

    func testRegistryCoversTheV1TypeSet() {
        XCTAssertEqual(
            TypeRegistry.all.map(\.key),
            [
                "sleepAnalysis", "workout", "stepCount", "heartRate",
                "restingHeartRate", "heartRateVariabilitySDNN",
                "activeEnergyBurned", "bodyMass",
            ]
        )
        XCTAssertEqual(TypeRegistry.readTypes.count, 8)
    }
}
