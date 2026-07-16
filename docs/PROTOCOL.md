# Pulso Ingest Protocol — v1

The contract between the Pulso iOS app and any receiving server. It is
deliberately small: a conforming server fits in ~50 lines of any language.
The reference implementation lives in [`server/server.py`](../server/server.py).

v1 is frozen. New *optional* sample fields may be added over time (additive
changes only); anything breaking becomes `/v2/ingest`.

Additive revisions so far:

- **v1.1 (2026-07-12)** — 49 new `type` keys (all listed below), full
  metadata pass-through on workouts, and the workout statistics fields
  (`totalBasalEnergyBurned`, `averageHeartRate`, `minimumHeartRate`,
  `maximumHeartRate`).

## Endpoints

### `POST /ingest`

The body is a **JSON array** of elements. Each element is either a **sample
object** or a **tombstone**:

```json
[
  {
    "uuid": "1E9B4C1A-3F2D-4C8E-9B7A-2D5E8F1A6C3B",
    "type": "sleepAnalysis",
    "start": "2026-07-06T01:12:00+01:00",
    "end": "2026-07-06T09:03:00+01:00",
    "value": "asleepREM",
    "source": "Lenilson's Apple Watch",
    "sourceBundleId": "com.apple.health.A1B2C3",
    "metadata": { "timeZone": "Europe/Lisbon" }
  },
  { "deleted": ["A0B1C2D3-...", "E4F5A6B7-..."] }
]
```

Requirements for a conforming server:

- **Accept `Content-Encoding: gzip`.** The app always gzips. (Python:
  `gzip.decompress(body)`; Node: `zlib.gunzipSync(body)`.)
- **Dedupe by `uuid`.** Delivery is at-least-once; re-sending any batch must
  be safe. `uuid` is the idempotency key.
- **Record tombstones.** Elements with a `deleted` key list uuids of samples
  removed from Apple Health (wrong workouts deleted, sleep rewritten, etc.).
  How you apply them is your business; don't lose them.
- **Respond `200` with a JSON object containing `"received": <n>`** (the
  element count). Extra fields are fine — the reference server also returns
  `"new"` and `"deleted"`.
- Auth is optional: if you set a token, require `Authorization: Bearer <token>`
  and reply `401` otherwise. Tailnet-only servers may run open.

Any non-2xx response (or no response) means the app keeps the batch on disk
and retries with exponential backoff (1 min → 1 h cap), in order, forever.
Batches are never dropped and never reordered.

### `GET /health`

Liveness probe; anything `200` passes. Used by the app's "Test Connection"
button.

## Sample object schema

Optional fields are **omitted** when absent (never `null`). Key order is not
significant.

| Field | Type | Notes |
|---|---|---|
| `uuid` | string | `HKObject.uuid` — the idempotency key |
| `type` | string | One of the type keys below |
| `start` | string | ISO 8601 with UTC offset, second precision |
| `end` | string | Same format; equals `start` for point samples |
| `value` | string \| number | Category name (e.g. `"asleepREM"`) or quantity number; absent for workouts and for categories with no meaningful value (`mindfulSession`) |
| `unit` | string | Only on quantity types; see tables below |
| `source` | string | Name of the writing device/app, verbatim |
| `sourceBundleId` | string | Bundle id of the writing app |
| `metadata` | object | See Metadata below |
| `workoutActivityType` | string | Workouts only, e.g. `"traditionalStrengthTraining"` |
| `duration` | number | Workouts only, seconds |
| `totalEnergyBurned` | number | Workouts only, active kcal |
| `totalBasalEnergyBurned` | number | Workouts only, basal kcal |
| `totalDistance` | number | Workouts only, meters |
| `averageHeartRate` | number | Workouts only, count/min |
| `minimumHeartRate` | number | Workouts only, count/min |
| `maximumHeartRate` | number | Workouts only, count/min |

### Metadata

- **Workouts** carry their sample's full HealthKit metadata dictionary —
  routine names (`HKWorkoutBrandName`), the indoor flag, weather, and
  whatever else the writing app attached. String, number, and boolean values
  pass through as native JSON; other value types (HKQuantity, dates) are
  stringified.
- **Every other type** carries at most one key: `timeZone`.
- In both cases the `timeZone` key means the sample *recorded* its timezone
  (see Timestamps); HealthKit's raw `HKTimeZone` key is folded into it, never
  duplicated.

### Type keys and units

Percent-unit types deliver human-scale values: `97` under unit `"%"` means
97% (HealthKit's internal 0–1 fractions are converted).

**Sleep & circadian**

| `type` | `value` | `unit` |
|---|---|---|
| `sleepAnalysis` | `inBed`, `awake`, `asleepUnspecified`, `asleepCore`, `asleepDeep`, `asleepREM` | — |
| `timeInDaylight` | number | `min` |
| `appleSleepingWristTemperature` | number | `°C` |
| `appleSleepingBreathingDisturbances` | number | `count` |
| `appleStandHour` | `stood`, `idle` | — |
| `mindfulSession` | — (interval is the data) | — |
| `stateOfMind` | number: valence in [−1, +1] | `valence` |

`stateOfMind` (iOS 18+) additionally carries in `metadata`: `kind`
(`momentaryEmotion` \| `dailyMood`), `valenceClassification`
(`veryUnpleasant` … `veryPleasant`), and, when present, `labels` and
`associations` as comma-joined name lists (e.g. `"happy,grateful"`,
`"family,work"`). Unknown future enum cases surface as `label_<n>` /
`association_<n>` / `kind_<n>` / `classification_<n>`.

**Workouts**

| `type` | `value` | `unit` |
|---|---|---|
| `workout` | — (see workout fields) | — |

**Energy & activity**

| `type` | `value` | `unit` |
|---|---|---|
| `stepCount` | number | `count` |
| `activeEnergyBurned` | number | `kcal` |
| `basalEnergyBurned` | number | `kcal` |
| `physicalEffort` | number | `kcal/hr·kg` |
| `appleExerciseTime` | number | `min` |
| `appleStandTime` | number | `min` |
| `flightsClimbed` | number | `count` |
| `distanceWalkingRunning` | number | `m` |
| `distanceCycling` | number | `m` |
| `distanceSwimming` | number | `m` |
| `swimmingStrokeCount` | number | `count` |

**Cardio & recovery**

| `type` | `value` | `unit` |
|---|---|---|
| `heartRate` | number | `count/min` |
| `restingHeartRate` | number | `count/min` |
| `heartRateVariabilitySDNN` | number | `ms` |
| `vo2Max` | number | `mL/(kg·min)` |
| `heartRateRecoveryOneMinute` | number | `count/min` |
| `walkingHeartRateAverage` | number | `count/min` |
| `oxygenSaturation` | number | `%` |
| `respiratoryRate` | number | `count/min` |

**Body**

| `type` | `value` | `unit` |
|---|---|---|
| `bodyMass` | number | `kg` |
| `bodyFatPercentage` | number | `%` |
| `leanBodyMass` | number | `kg` |
| `bodyMassIndex` | number | `count` |
| `waistCircumference` | number | `cm` |
| `height` | number | `cm` |

**Running & gait**

| `type` | `value` | `unit` |
|---|---|---|
| `runningPower` | number | `W` |
| `runningSpeed` | number | `m/s` |
| `runningStrideLength` | number | `m` |
| `runningVerticalOscillation` | number | `cm` |
| `runningGroundContactTime` | number | `ms` |
| `walkingSpeed` | number | `m/s` |
| `walkingStepLength` | number | `cm` |
| `walkingAsymmetryPercentage` | number | `%` |
| `walkingDoubleSupportPercentage` | number | `%` |
| `appleWalkingSteadiness` | number | `%` |
| `sixMinuteWalkTestDistance` | number | `m` |
| `stairAscentSpeed` | number | `m/s` |
| `stairDescentSpeed` | number | `m/s` |

**Audio exposure**

| `type` | `value` | `unit` |
|---|---|---|
| `environmentalAudioExposure` | number | `dBASPL` |
| `headphoneAudioExposure` | number | `dBASPL` |

**Nutrition**

| `type` | `value` | `unit` |
|---|---|---|
| `dietaryEnergyConsumed` | number | `kcal` |
| `dietaryProtein` | number | `g` |
| `dietaryCarbohydrates` | number | `g` |
| `dietaryFatTotal` | number | `g` |
| `dietaryFiber` | number | `g` |
| `dietarySugar` | number | `g` |
| `dietarySodium` | number | `mg` |
| `dietaryWater` | number | `mL` |
| `dietaryCaffeine` | number | `mg` |
| `numberOfAlcoholicBeverages` | number | `count` |

Unknown category values serialize as `"value_<n>"`; unknown workout
activities as `"activity_<n>"` — data is never silently merged into "other".

## Timestamps and timezones

Timestamps are **ISO 8601 with the sample's own UTC offset — never
normalized to UTC**. Apple's own `export.xml` stamps all history with the
export-time offset, destroying original timezones; Pulso does not repeat
that mistake.

- If the sample recorded a timezone (`HKMetadataKeyTimeZone`), its offset is
  used **and** `metadata.timeZone` carries the zone identifier.
- Otherwise the device's timezone at sync time is used and `metadata` is
  **absent** — so consumers can distinguish a recorded timezone from an
  assumed one.

## Delivery semantics

- Batches are at most 5,000 samples / ~2 MB gzipped.
- Order: batches arrive in the order the app read them (FIFO per queue).
- At-least-once: duplicates are possible (crash between upload and local
  bookkeeping); the `uuid` dedupe rule makes the stream effectively
  exactly-once.
- Sources are **not** merged client-side: Apple Watch, WHOOP, AutoSleep etc.
  each deliver their own overlapping records, faithfully attributed via
  `source`. Merging is analysis-side policy, not transport policy.

## Reference server storage (informative)

`server/server.py` appends each element to `data/<type>.ndjson` (tombstones
to `data/_deleted.ndjson`), one JSON object per line, adding a `receivedAt`
field with the server's local time. Inspect with `cat`, `jq`, or anything
that reads lines.
