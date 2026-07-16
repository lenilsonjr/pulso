import Foundation
import HealthKit

/// One syncable HealthKit type: its wire key, how eagerly HealthKit may wake
/// us for it, and how its samples serialize. Adding a type is a table row.
struct SyncedType: Identifiable {
    let key: String // wire "type" value and persistence key
    let displayName: String
    let group: String // UI section
    let sampleType: HKSampleType
    let frequency: HKUpdateFrequency
    let serialize: @Sendable (HKSample, SerializerContext) -> SampleDTO?

    var id: String { key }
}

enum TypeRegistry {
    enum Group {
        static let sleep = "Sleep & Circadian"
        static let workouts = "Workouts"
        static let energy = "Energy & Activity"
        static let cardio = "Cardio & Recovery"
        static let body = "Body"
        static let gait = "Running & Gait"
        static let audio = "Audio Exposure"
        static let nutrition = "Nutrition"
    }

    private static let bpm = HKUnit.count().unitDivided(by: .minute())
    private static let mps = HKUnit.meter().unitDivided(by: .second())

    /// Shorthand: one registry row per quantity type. Everything defaults to
    /// .hourly — HealthKit's cap for most quantity types anyway.
    private static func quantity(
        _ identifier: HKQuantityTypeIdentifier, key: String, name: String, group: String,
        unit: HKUnit, label: String, scale: Double = 1
    ) -> SyncedType {
        SyncedType(
            key: key, displayName: name, group: group,
            sampleType: HKQuantityType(identifier), frequency: .hourly,
            serialize: Serializers.quantity(type: key, unit: unit, unitLabel: label, scale: scale)
        )
    }

    // MARK: - Groups
    // Sleep and workouts are what this app exists for — they get .immediate
    // background delivery. Everything else is capped at .hourly by HealthKit.

    private static let sleepAndCircadian: [SyncedType] = {
        var types: [SyncedType] = [
            SyncedType(
                key: "sleepAnalysis", displayName: "Sleep", group: Group.sleep,
                sampleType: HKCategoryType(.sleepAnalysis),
                frequency: .immediate, serialize: Serializers.sleep
            ),
            quantity(.timeInDaylight, key: "timeInDaylight", name: "Time in Daylight",
                     group: Group.sleep, unit: .minute(), label: "min"),
            quantity(.appleSleepingWristTemperature, key: "appleSleepingWristTemperature",
                     name: "Sleeping Wrist Temperature", group: Group.sleep,
                     unit: .degreeCelsius(), label: "°C"),
        ]
        if #available(iOS 18.0, *) {
            types.append(quantity(.appleSleepingBreathingDisturbances, key: "appleSleepingBreathingDisturbances",
                                  name: "Breathing Disturbances", group: Group.sleep,
                                  unit: .count(), label: "count"))
            types.append(SyncedType(
                key: "stateOfMind", displayName: "State of Mind", group: Group.sleep,
                sampleType: HKObjectType.stateOfMindType(),
                frequency: .hourly, serialize: Serializers.stateOfMind
            ))
        }
        types += [
            SyncedType(
                key: "appleStandHour", displayName: "Stand Hours", group: Group.sleep,
                sampleType: HKCategoryType(.appleStandHour),
                frequency: .hourly,
                serialize: Serializers.category(type: "appleStandHour", valueName: Serializers.standHourName)
            ),
            SyncedType(
                key: "mindfulSession", displayName: "Mindful Sessions", group: Group.sleep,
                sampleType: HKCategoryType(.mindfulSession),
                frequency: .hourly,
                // Value is notApplicable; the interval itself is the session.
                serialize: Serializers.category(type: "mindfulSession", valueName: { _ in nil })
            ),
        ]
        return types
    }()

    private static let workouts: [SyncedType] = [
        SyncedType(
            key: "workout", displayName: "Workouts", group: Group.workouts,
            sampleType: HKObjectType.workoutType(),
            frequency: .immediate, serialize: Serializers.workout
        ),
    ]

    private static let energyAndActivity: [SyncedType] = [
        quantity(.stepCount, key: "stepCount", name: "Steps",
                 group: Group.energy, unit: .count(), label: "count"),
        quantity(.activeEnergyBurned, key: "activeEnergyBurned", name: "Active Energy",
                 group: Group.energy, unit: .kilocalorie(), label: "kcal"),
        quantity(.basalEnergyBurned, key: "basalEnergyBurned", name: "Basal Energy",
                 group: Group.energy, unit: .kilocalorie(), label: "kcal"),
        quantity(.physicalEffort, key: "physicalEffort", name: "Physical Effort",
                 group: Group.energy,
                 unit: .kilocalorie().unitDivided(by: .hour().unitMultiplied(by: .gramUnit(with: .kilo))),
                 label: "kcal/hr·kg"),
        quantity(.appleExerciseTime, key: "appleExerciseTime", name: "Exercise Time",
                 group: Group.energy, unit: .minute(), label: "min"),
        quantity(.appleStandTime, key: "appleStandTime", name: "Stand Time",
                 group: Group.energy, unit: .minute(), label: "min"),
        quantity(.flightsClimbed, key: "flightsClimbed", name: "Flights Climbed",
                 group: Group.energy, unit: .count(), label: "count"),
        quantity(.distanceWalkingRunning, key: "distanceWalkingRunning", name: "Walking + Running Distance",
                 group: Group.energy, unit: .meter(), label: "m"),
        quantity(.distanceCycling, key: "distanceCycling", name: "Cycling Distance",
                 group: Group.energy, unit: .meter(), label: "m"),
        quantity(.distanceSwimming, key: "distanceSwimming", name: "Swimming Distance",
                 group: Group.energy, unit: .meter(), label: "m"),
        quantity(.swimmingStrokeCount, key: "swimmingStrokeCount", name: "Swimming Strokes",
                 group: Group.energy, unit: .count(), label: "count"),
    ]

    private static let cardioAndRecovery: [SyncedType] = [
        quantity(.heartRate, key: "heartRate", name: "Heart Rate",
                 group: Group.cardio, unit: bpm, label: "count/min"),
        quantity(.restingHeartRate, key: "restingHeartRate", name: "Resting Heart Rate",
                 group: Group.cardio, unit: bpm, label: "count/min"),
        quantity(.heartRateVariabilitySDNN, key: "heartRateVariabilitySDNN", name: "HRV (SDNN)",
                 group: Group.cardio, unit: .secondUnit(with: .milli), label: "ms"),
        quantity(.vo2Max, key: "vo2Max", name: "VO₂ Max",
                 group: Group.cardio,
                 unit: .literUnit(with: .milli)
                     .unitDivided(by: .gramUnit(with: .kilo).unitMultiplied(by: .minute())),
                 label: "mL/(kg·min)"),
        quantity(.heartRateRecoveryOneMinute, key: "heartRateRecoveryOneMinute", name: "Heart Rate Recovery",
                 group: Group.cardio, unit: bpm, label: "count/min"),
        quantity(.walkingHeartRateAverage, key: "walkingHeartRateAverage", name: "Walking Heart Rate Avg",
                 group: Group.cardio, unit: bpm, label: "count/min"),
        quantity(.oxygenSaturation, key: "oxygenSaturation", name: "Blood Oxygen",
                 group: Group.cardio, unit: .percent(), label: "%", scale: 100),
        quantity(.respiratoryRate, key: "respiratoryRate", name: "Respiratory Rate",
                 group: Group.cardio, unit: bpm, label: "count/min"),
    ]

    private static let body: [SyncedType] = [
        quantity(.bodyMass, key: "bodyMass", name: "Body Mass",
                 group: Group.body, unit: .gramUnit(with: .kilo), label: "kg"),
        quantity(.bodyFatPercentage, key: "bodyFatPercentage", name: "Body Fat",
                 group: Group.body, unit: .percent(), label: "%", scale: 100),
        quantity(.leanBodyMass, key: "leanBodyMass", name: "Lean Body Mass",
                 group: Group.body, unit: .gramUnit(with: .kilo), label: "kg"),
        quantity(.bodyMassIndex, key: "bodyMassIndex", name: "Body Mass Index",
                 group: Group.body, unit: .count(), label: "count"),
        quantity(.waistCircumference, key: "waistCircumference", name: "Waist Circumference",
                 group: Group.body, unit: .meterUnit(with: .centi), label: "cm"),
        quantity(.height, key: "height", name: "Height",
                 group: Group.body, unit: .meterUnit(with: .centi), label: "cm"),
    ]

    private static let runningAndGait: [SyncedType] = [
        quantity(.runningPower, key: "runningPower", name: "Running Power",
                 group: Group.gait, unit: .watt(), label: "W"),
        quantity(.runningSpeed, key: "runningSpeed", name: "Running Speed",
                 group: Group.gait, unit: mps, label: "m/s"),
        quantity(.runningStrideLength, key: "runningStrideLength", name: "Running Stride Length",
                 group: Group.gait, unit: .meter(), label: "m"),
        quantity(.runningVerticalOscillation, key: "runningVerticalOscillation", name: "Vertical Oscillation",
                 group: Group.gait, unit: .meterUnit(with: .centi), label: "cm"),
        quantity(.runningGroundContactTime, key: "runningGroundContactTime", name: "Ground Contact Time",
                 group: Group.gait, unit: .secondUnit(with: .milli), label: "ms"),
        quantity(.walkingSpeed, key: "walkingSpeed", name: "Walking Speed",
                 group: Group.gait, unit: mps, label: "m/s"),
        quantity(.walkingStepLength, key: "walkingStepLength", name: "Walking Step Length",
                 group: Group.gait, unit: .meterUnit(with: .centi), label: "cm"),
        quantity(.walkingAsymmetryPercentage, key: "walkingAsymmetryPercentage", name: "Walking Asymmetry",
                 group: Group.gait, unit: .percent(), label: "%", scale: 100),
        quantity(.walkingDoubleSupportPercentage, key: "walkingDoubleSupportPercentage", name: "Double Support",
                 group: Group.gait, unit: .percent(), label: "%", scale: 100),
        quantity(.appleWalkingSteadiness, key: "appleWalkingSteadiness", name: "Walking Steadiness",
                 group: Group.gait, unit: .percent(), label: "%", scale: 100),
        quantity(.sixMinuteWalkTestDistance, key: "sixMinuteWalkTestDistance", name: "Six-Minute Walk",
                 group: Group.gait, unit: .meter(), label: "m"),
        quantity(.stairAscentSpeed, key: "stairAscentSpeed", name: "Stair Ascent Speed",
                 group: Group.gait, unit: mps, label: "m/s"),
        quantity(.stairDescentSpeed, key: "stairDescentSpeed", name: "Stair Descent Speed",
                 group: Group.gait, unit: mps, label: "m/s"),
    ]

    private static let audioExposure: [SyncedType] = [
        quantity(.environmentalAudioExposure, key: "environmentalAudioExposure", name: "Environmental Sound",
                 group: Group.audio, unit: .decibelAWeightedSoundPressureLevel(), label: "dBASPL"),
        quantity(.headphoneAudioExposure, key: "headphoneAudioExposure", name: "Headphone Audio",
                 group: Group.audio, unit: .decibelAWeightedSoundPressureLevel(), label: "dBASPL"),
    ]

    private static let nutrition: [SyncedType] = [
        quantity(.dietaryEnergyConsumed, key: "dietaryEnergyConsumed", name: "Dietary Energy",
                 group: Group.nutrition, unit: .kilocalorie(), label: "kcal"),
        quantity(.dietaryProtein, key: "dietaryProtein", name: "Protein",
                 group: Group.nutrition, unit: .gram(), label: "g"),
        quantity(.dietaryCarbohydrates, key: "dietaryCarbohydrates", name: "Carbohydrates",
                 group: Group.nutrition, unit: .gram(), label: "g"),
        quantity(.dietaryFatTotal, key: "dietaryFatTotal", name: "Total Fat",
                 group: Group.nutrition, unit: .gram(), label: "g"),
        quantity(.dietaryFiber, key: "dietaryFiber", name: "Fiber",
                 group: Group.nutrition, unit: .gram(), label: "g"),
        quantity(.dietarySugar, key: "dietarySugar", name: "Sugar",
                 group: Group.nutrition, unit: .gram(), label: "g"),
        quantity(.dietarySodium, key: "dietarySodium", name: "Sodium",
                 group: Group.nutrition, unit: .gramUnit(with: .milli), label: "mg"),
        quantity(.dietaryWater, key: "dietaryWater", name: "Water",
                 group: Group.nutrition, unit: .literUnit(with: .milli), label: "mL"),
        quantity(.dietaryCaffeine, key: "dietaryCaffeine", name: "Caffeine",
                 group: Group.nutrition, unit: .gramUnit(with: .milli), label: "mg"),
        quantity(.numberOfAlcoholicBeverages, key: "numberOfAlcoholicBeverages", name: "Alcoholic Beverages",
                 group: Group.nutrition, unit: .count(), label: "count"),
    ]

    // MARK: - Public surface

    static let all: [SyncedType] =
        sleepAndCircadian + workouts + energyAndActivity + cardioAndRecovery
            + body + runningAndGait + audioExposure + nutrition

    /// Group names in registry order, for UI sections.
    static let groups: [String] = {
        var seen = Set<String>()
        return all.compactMap { seen.insert($0.group).inserted ? $0.group : nil }
    }()

    static func type(for key: String) -> SyncedType? {
        all.first { $0.key == key }
    }

    static var readTypes: Set<HKObjectType> {
        Set(all.map { $0.sampleType as HKObjectType })
    }
}
