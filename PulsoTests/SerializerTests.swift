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
        XCTAssertEqual(dto.metadata, ["timeZone": .string("Europe/Lisbon")])
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
        // Synthetic workout carries no statistics or metadata; fields stay
        // absent rather than 0 / empty.
        XCTAssertNil(dto.totalEnergyBurned)
        XCTAssertNil(dto.totalBasalEnergyBurned)
        XCTAssertNil(dto.totalDistance)
        XCTAssertNil(dto.averageHeartRate)
        XCTAssertNil(dto.minimumHeartRate)
        XCTAssertNil(dto.maximumHeartRate)
        XCTAssertNil(dto.metadata)
    }

    func testWorkoutMetadataPassThroughAndLegacyTotals() throws {
        let start = TestSupport.date(2026, 7, 5, 18, 30, zone: "Europe/Lisbon")
        // .running, not strength training: HealthKit only keeps a total
        // distance when the activity has an associated distance type.
        let workout = HKWorkout(
            activityType: .running,
            start: start,
            end: start.addingTimeInterval(3600),
            workoutEvents: nil,
            totalEnergyBurned: HKQuantity(unit: .kilocalorie(), doubleValue: 412),
            totalDistance: HKQuantity(unit: .meter(), doubleValue: 5000),
            metadata: [
                "HKWorkoutBrandName": "Push Day A",
                HKMetadataKeyIndoorWorkout: true,
                "customRounds": NSNumber(value: 3),
                HKMetadataKeyTimeZone: "Europe/Lisbon",
            ]
        )

        let dto = try XCTUnwrap(Serializers.workout(workout, context: context))
        // Full metadata passes through; HKTimeZone is folded into "timeZone".
        XCTAssertEqual(dto.metadata?["HKWorkoutBrandName"], .string("Push Day A"))
        XCTAssertEqual(dto.metadata?[HKMetadataKeyIndoorWorkout], .bool(true))
        XCTAssertEqual(dto.metadata?["customRounds"], .number(3))
        XCTAssertEqual(dto.metadata?["timeZone"], .string("Europe/Lisbon"))
        XCTAssertNil(dto.metadata?[HKMetadataKeyTimeZone])
        XCTAssertEqual(dto.start, "2026-07-05T18:30:00+01:00", "recorded zone must drive the offset")
        // Legacy totals (pre-iOS-16 recordings) still surface when statistics are absent.
        XCTAssertEqual(dto.totalEnergyBurned ?? 0, 412, accuracy: 0.001)
        XCTAssertEqual(dto.totalDistance ?? 0, 5000, accuracy: 0.001)
    }

    func testJsonValueConversion() {
        XCTAssertEqual(Serializers.jsonValue("text", context: context), .string("text"))
        XCTAssertEqual(Serializers.jsonValue(NSNumber(value: 42.5), context: context), .number(42.5))
        XCTAssertEqual(Serializers.jsonValue(true, context: context), .bool(true))
        XCTAssertEqual(Serializers.jsonValue(NSNumber(value: 1), context: context), .number(1),
                       "NSNumber(1) must stay a number, not collapse into a bool")
        // HKQuantity and other exotic values are stringified, not dropped.
        let quantity = HKQuantity(unit: .kilocalorie(), doubleValue: 412)
        guard case .string(let s)? = Serializers.jsonValue(quantity, context: context) as JSONValue? else {
            return XCTFail("expected a stringified quantity")
        }
        XCTAssertTrue(s.contains("412"), s)
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

    func testPercentTypesDeliverHumanScaleValues() throws {
        // HealthKit reads percent quantities as fractions (0–1); the wire
        // carries 0–100 under a "%" label.
        let start = TestSupport.date(2026, 7, 6, 4, 0, zone: "Asia/Dubai")
        let sample = HKQuantitySample(
            type: HKQuantityType(.oxygenSaturation),
            quantity: HKQuantity(unit: .percent(), doubleValue: 0.97),
            start: start,
            end: start
        )
        let serialize = try XCTUnwrap(TypeRegistry.type(for: "oxygenSaturation")).serialize
        let dto = try XCTUnwrap(serialize(sample, context))
        XCTAssertEqual(dto.value, .number(97))
        XCTAssertEqual(dto.unit, "%")
    }

    func testUnitConversions() throws {
        let start = TestSupport.date(2026, 7, 6, 4, 0, zone: "Asia/Dubai")
        // Exercise time written in seconds must arrive in minutes.
        let exercise = HKQuantitySample(
            type: HKQuantityType(.appleExerciseTime),
            quantity: HKQuantity(unit: .second(), doubleValue: 120),
            start: start, end: start
        )
        var dto = try XCTUnwrap(TypeRegistry.type(for: "appleExerciseTime")?.serialize(exercise, context))
        XCTAssertEqual(dto.value, .number(2))
        XCTAssertEqual(dto.unit, "min")

        // Wrist temperature written in Fahrenheit must arrive in Celsius.
        let temperature = HKQuantitySample(
            type: HKQuantityType(.appleSleepingWristTemperature),
            quantity: HKQuantity(unit: .degreeFahrenheit(), doubleValue: 98.6),
            start: start, end: start
        )
        dto = try XCTUnwrap(TypeRegistry.type(for: "appleSleepingWristTemperature")?.serialize(temperature, context))
        guard case .number(let celsius)? = dto.value else { return XCTFail("expected a number") }
        XCTAssertEqual(celsius, 37, accuracy: 0.01)
        XCTAssertEqual(dto.unit, "°C")

        // Ground contact time written in seconds must arrive in milliseconds.
        let contact = HKQuantitySample(
            type: HKQuantityType(.runningGroundContactTime),
            quantity: HKQuantity(unit: .second(), doubleValue: 0.25),
            start: start, end: start
        )
        dto = try XCTUnwrap(TypeRegistry.type(for: "runningGroundContactTime")?.serialize(contact, context))
        XCTAssertEqual(dto.value, .number(250))
        XCTAssertEqual(dto.unit, "ms")
    }

    func testStandHourAndMindfulSessionCategories() throws {
        let start = TestSupport.date(2026, 7, 6, 10, 0, zone: "Asia/Dubai")
        let stood = HKCategorySample(
            type: HKCategoryType(.appleStandHour),
            value: HKCategoryValueAppleStandHour.stood.rawValue,
            start: start, end: start.addingTimeInterval(3600)
        )
        var dto = try XCTUnwrap(TypeRegistry.type(for: "appleStandHour")?.serialize(stood, context))
        XCTAssertEqual(dto.value, .string("stood"))

        let idle = HKCategorySample(
            type: HKCategoryType(.appleStandHour),
            value: HKCategoryValueAppleStandHour.idle.rawValue,
            start: start, end: start.addingTimeInterval(3600)
        )
        dto = try XCTUnwrap(TypeRegistry.type(for: "appleStandHour")?.serialize(idle, context))
        XCTAssertEqual(dto.value, .string("idle"))

        // Mindful sessions carry no meaningful category value: the interval
        // is the data, so `value` is omitted.
        let mindful = HKCategorySample(
            type: HKCategoryType(.mindfulSession),
            value: 0,
            start: start, end: start.addingTimeInterval(600)
        )
        dto = try XCTUnwrap(TypeRegistry.type(for: "mindfulSession")?.serialize(mindful, context))
        XCTAssertNil(dto.value)
        XCTAssertEqual(dto.type, "mindfulSession")
    }

    /// The wire type keys are protocol surface — pin the full v1.1 set.
    func testRegistryCoversTheV11TypeSet() {
        var expected = [
            // Sleep & Circadian
            "sleepAnalysis", "timeInDaylight", "appleSleepingWristTemperature",
        ]
        if #available(iOS 18.0, *) {
            expected.append("appleSleepingBreathingDisturbances")
            expected.append("stateOfMind")
        }
        expected += [
            "appleStandHour", "mindfulSession",
            // Workouts
            "workout",
            // Energy & Activity
            "stepCount", "activeEnergyBurned", "basalEnergyBurned", "physicalEffort",
            "appleExerciseTime", "appleStandTime", "flightsClimbed",
            "distanceWalkingRunning", "distanceCycling", "distanceSwimming", "swimmingStrokeCount",
            // Cardio & Recovery
            "heartRate", "restingHeartRate", "heartRateVariabilitySDNN", "vo2Max",
            "heartRateRecoveryOneMinute", "walkingHeartRateAverage", "oxygenSaturation", "respiratoryRate",
            // Body
            "bodyMass", "bodyFatPercentage", "leanBodyMass", "bodyMassIndex",
            "waistCircumference", "height",
            // Running & Gait
            "runningPower", "runningSpeed", "runningStrideLength", "runningVerticalOscillation",
            "runningGroundContactTime", "walkingSpeed", "walkingStepLength",
            "walkingAsymmetryPercentage", "walkingDoubleSupportPercentage", "appleWalkingSteadiness",
            "sixMinuteWalkTestDistance", "stairAscentSpeed", "stairDescentSpeed",
            // Audio Exposure
            "environmentalAudioExposure", "headphoneAudioExposure",
            // Nutrition
            "dietaryEnergyConsumed", "dietaryProtein", "dietaryCarbohydrates", "dietaryFatTotal",
            "dietaryFiber", "dietarySugar", "dietarySodium", "dietaryWater", "dietaryCaffeine",
            "numberOfAlcoholicBeverages",
        ]
        XCTAssertEqual(TypeRegistry.all.map(\.key), expected)
        XCTAssertEqual(Set(TypeRegistry.all.map(\.key)).count, TypeRegistry.all.count, "keys must be unique")
        XCTAssertEqual(TypeRegistry.readTypes.count, TypeRegistry.all.count)
        XCTAssertEqual(TypeRegistry.groups.count, 8)
        // Only the app's raison-d'être types warrant .immediate wakes.
        XCTAssertEqual(
            TypeRegistry.all.filter { $0.frequency == .immediate }.map(\.key),
            ["sleepAnalysis", "workout"]
        )
    }
}
