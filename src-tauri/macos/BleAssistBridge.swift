import CoreBluetooth
import Dispatch
import Foundation

struct BleAssistCapsule: Codable {
    let version: UInt8
    let issued_at_unix_ms: UInt64
    let expires_at_unix_ms: UInt64
    let rolling_identifier: String
    let integrity_tag: String
    let transport_hint: String
    let qr_fallback_available: Bool
    let short_code_fallback_available: Bool
    let rotation_window_ms: UInt64
}

struct BleAdvertisementRequest: Codable {
    let updated_at_unix_ms: UInt64
    let capsule: BleAssistCapsule
}

struct BridgeSnapshot: Codable {
    var permission_state: String?
    var scanner_state: String?
    var advertiser_state: String?
    var advertised_rolling_identifier: String?
    var capsules: [BleAssistCapsule]
}

enum CompactBleAdvertisementCodec {
    static let companyIdLo: UInt8 = 0xFF
    static let companyIdHi: UInt8 = 0xFF
    static let rollingBytes = 7
    static let integrityBytes = 7

    static func encode(_ capsule: BleAssistCapsule) -> Data? {
        guard let rolling = decodeBase64URL(capsule.rolling_identifier),
              let integrity = decodeBase64URL(capsule.integrity_tag),
              rolling.count >= rollingBytes,
              integrity.count >= integrityBytes else {
            return nil
        }

        let issuedAtSecs = UInt32(min(capsule.issued_at_unix_ms / 1000, UInt64(UInt32.max)))
        let ttlSecs = UInt16(
            min(
                capsule.expires_at_unix_ms.saturatingSubtract(capsule.issued_at_unix_ms) / 1000,
                UInt64(UInt16.max)
            )
        )
        let flags: UInt8 =
            (capsule.qr_fallback_available ? 0x01 : 0x00)
            | (capsule.short_code_fallback_available ? 0x02 : 0x00)

        var payload = Data()
        payload.append(companyIdLo)
        payload.append(companyIdHi)
        payload.append(capsule.version)
        payload.append(flags)
        payload.append(contentsOf: withUnsafeBytes(of: issuedAtSecs.littleEndian, Array.init))
        payload.append(contentsOf: withUnsafeBytes(of: ttlSecs.littleEndian, Array.init))
        payload.append(rolling.prefix(rollingBytes))
        payload.append(integrity.prefix(integrityBytes))
        return payload
    }

    static func decode(_ data: Data) -> BleAssistCapsule? {
        let minimumLength = 2 + 1 + 1 + 4 + 2 + rollingBytes + integrityBytes
        guard data.count >= minimumLength else {
            return nil
        }
        guard data[0] == companyIdLo, data[1] == companyIdHi else {
            return nil
        }

        let version = data[2]
        let flags = data[3]
        let issuedAtSecs = data.subdata(in: 4..<8).withUnsafeBytes { rawBuffer in
            rawBuffer.load(as: UInt32.self)
        }.littleEndian
        let ttlSecs = data.subdata(in: 8..<10).withUnsafeBytes { rawBuffer in
            rawBuffer.load(as: UInt16.self)
        }.littleEndian
        let rollingStart = 10
        let integrityStart = rollingStart + rollingBytes
        let rollingIdentifier = encodeBase64URL(data.subdata(in: rollingStart..<integrityStart))
        let integrityTag = encodeBase64URL(
            data.subdata(in: integrityStart..<(integrityStart + integrityBytes))
        )
        let issuedAtUnixMs = UInt64(issuedAtSecs) * 1000
        let expiresAtUnixMs = issuedAtUnixMs + (UInt64(ttlSecs) * 1000)

        return BleAssistCapsule(
            version: version,
            issued_at_unix_ms: issuedAtUnixMs,
            expires_at_unix_ms: expiresAtUnixMs,
            rolling_identifier: rollingIdentifier,
            integrity_tag: integrityTag,
            transport_hint: "ble_manufacturer_data",
            qr_fallback_available: (flags & 0x01) != 0,
            short_code_fallback_available: (flags & 0x02) != 0,
            rotation_window_ms: 30_000
        )
    }

    private static func decodeBase64URL(_ value: String) -> Data? {
        var normalized = value
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = normalized.count % 4
        if remainder != 0 {
            normalized += String(repeating: "=", count: 4 - remainder)
        }
        return Data(base64Encoded: normalized)
    }

    private static func encodeBase64URL(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

private extension UInt64 {
    func saturatingSubtract(_ other: UInt64) -> UInt64 {
        self > other ? self - other : 0
    }
}

final class BridgeWriter {
    private let snapshotURL: URL
    private let encoder: JSONEncoder

    init(snapshotURL: URL) {
        self.snapshotURL = snapshotURL
        self.encoder = JSONEncoder()
        self.encoder.outputFormatting = [.sortedKeys]
    }

    func write(_ snapshot: BridgeSnapshot) {
        do {
            let parent = snapshotURL.deletingLastPathComponent()
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
            let data = try encoder.encode(snapshot)
            try data.write(to: snapshotURL, options: .atomic)
        } catch {
            fputs("dashdrop ble bridge write failed: \(error)\n", stderr)
        }
    }
}

final class BleBridgeRuntime: NSObject, CBCentralManagerDelegate, CBPeripheralManagerDelegate {
    private let queue = DispatchQueue(label: "com.young.dashdrop.ble.bridge")
    private let writer: BridgeWriter
    private let advertisementURL: URL
    private let parentPID: Int32

    private var central: CBCentralManager?
    private var peripheral: CBPeripheralManager?
    private var capsules: [String: BleAssistCapsule] = [:]
    private var scannerState = "bridge_initializing"
    private var advertiserState = "bridge_advertiser_initializing"
    private var isScanning = false
    private var pruneTimer: DispatchSourceTimer?
    private var parentTimer: DispatchSourceTimer?
    private var advertisementPollTimer: DispatchSourceTimer?
    private var lastAdvertisementPayload: Data?
    private var advertisedRollingIdentifier: String?

    init(snapshotURL: URL, advertisementURL: URL) {
        self.writer = BridgeWriter(snapshotURL: snapshotURL)
        self.advertisementURL = advertisementURL
        self.parentPID = getppid()
        super.init()
        self.central = CBCentralManager(delegate: self, queue: queue)
        self.peripheral = CBPeripheralManager(delegate: self, queue: queue)
        scheduleMaintenanceTimers()
        writeSnapshot()
    }

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        switch central.state {
        case .poweredOn:
            scannerState = "bridge_authorized_idle"
            startScanningIfNeeded()
        case .poweredOff:
            scannerState = "bridge_powered_off"
            isScanning = false
        case .unsupported:
            scannerState = "bridge_unsupported"
            isScanning = false
        case .unauthorized:
            scannerState = "bridge_permission_denied"
            isScanning = false
        case .resetting:
            scannerState = "bridge_resetting"
            isScanning = false
        case .unknown:
            fallthrough
        @unknown default:
            scannerState = "bridge_initializing"
            isScanning = false
        }
        refreshAdvertisingFromDisk()
        writeSnapshot()
    }

    func peripheralManagerDidUpdateState(_ peripheral: CBPeripheralManager) {
        switch peripheral.state {
        case .poweredOn:
            advertiserState = "bridge_advertiser_ready"
            refreshAdvertisingFromDisk()
        case .poweredOff:
            advertiserState = "bridge_advertiser_powered_off"
        case .unsupported:
            advertiserState = "bridge_advertiser_unsupported"
        case .unauthorized:
            advertiserState = "bridge_advertiser_unauthorized"
        case .resetting:
            advertiserState = "bridge_advertiser_resetting"
        case .unknown:
            fallthrough
        @unknown default:
            advertiserState = "bridge_advertiser_initializing"
        }
        writeSnapshot()
    }

    func centralManager(
        _ central: CBCentralManager,
        didDiscover peripheral: CBPeripheral,
        advertisementData: [String: Any],
        rssi RSSI: NSNumber
    ) {
        if let capsule = decodeCapsule(from: advertisementData) {
            capsules[capsule.rolling_identifier] = capsule
        }
        pruneExpiredCapsules()
        writeSnapshot()
    }

    private func startScanningIfNeeded() {
        guard let central, !isScanning else { return }
        central.scanForPeripherals(withServices: nil, options: [
            CBCentralManagerScanOptionAllowDuplicatesKey: true
        ])
        isScanning = true
        scannerState = "bridge_scanning"
        writeSnapshot()
    }

    private func decodeCapsule(from advertisementData: [String: Any]) -> BleAssistCapsule? {
        if let manufacturerData = advertisementData[CBAdvertisementDataManufacturerDataKey] as? Data,
           let capsule = CompactBleAdvertisementCodec.decode(manufacturerData) {
            return capsule
        }

        if let serviceData = advertisementData[CBAdvertisementDataServiceDataKey] as? [CBUUID: Data] {
            for data in serviceData.values {
                if let capsule = try? JSONDecoder().decode(BleAssistCapsule.self, from: data) {
                    return capsule
                }
            }
        }

        return nil
    }

    private func refreshAdvertisingFromDisk() {
        guard let peripheral else { return }

        let request: BleAdvertisementRequest?
        do {
            let raw = try Data(contentsOf: advertisementURL)
            if raw.isEmpty {
                request = nil
            } else {
                request = try JSONDecoder().decode(BleAdvertisementRequest.self, from: raw)
            }
        } catch {
            request = nil
        }

        guard let request else {
            if peripheral.isAdvertising {
                peripheral.stopAdvertising()
            }
            if peripheral.state == .poweredOn {
                advertiserState = "observer_only_bridge"
            }
            lastAdvertisementPayload = nil
            advertisedRollingIdentifier = nil
            writeSnapshot()
            return
        }

        let now = nowMillis()
        guard request.capsule.expires_at_unix_ms > now else {
            if peripheral.isAdvertising {
                peripheral.stopAdvertising()
            }
            advertiserState = "bridge_advertisement_expired"
            lastAdvertisementPayload = nil
            advertisedRollingIdentifier = nil
            writeSnapshot()
            return
        }

        guard peripheral.state == .poweredOn else {
            advertiserState = "bridge_advertisement_pending_power"
            advertisedRollingIdentifier = request.capsule.rolling_identifier
            writeSnapshot()
            return
        }

        guard let payload = CompactBleAdvertisementCodec.encode(request.capsule) else {
            advertiserState = "bridge_advertisement_encode_failed"
            writeSnapshot()
            return
        }

        if lastAdvertisementPayload == payload, peripheral.isAdvertising {
            advertiserState = "bridge_advertising_capsule"
            advertisedRollingIdentifier = request.capsule.rolling_identifier
            writeSnapshot()
            return
        }

        if peripheral.isAdvertising {
            peripheral.stopAdvertising()
        }

        peripheral.startAdvertising([
            CBAdvertisementDataManufacturerDataKey: payload
        ])
        lastAdvertisementPayload = payload
        advertiserState = "bridge_advertising_capsule"
        advertisedRollingIdentifier = request.capsule.rolling_identifier
        writeSnapshot()
    }

    private func pruneExpiredCapsules() {
        let now = nowMillis()
        capsules = capsules.filter { _, capsule in
            capsule.expires_at_unix_ms > now
        }
    }

    private func writeSnapshot() {
        let snapshot = BridgeSnapshot(
            permission_state: permissionState(),
            scanner_state: scannerState,
            advertiser_state: advertiserState,
            advertised_rolling_identifier: advertisedRollingIdentifier,
            capsules: Array(capsules.values).sorted { lhs, rhs in
                lhs.rolling_identifier < rhs.rolling_identifier
            }
        )
        writer.write(snapshot)
    }

    private func permissionState() -> String {
        if #available(macOS 10.15, *) {
            switch CBCentralManager.authorization {
            case .allowedAlways:
                return "granted"
            case .denied:
                return "denied"
            case .restricted:
                return "restricted"
            case .notDetermined:
                return "prompt_required"
            @unknown default:
                return "unknown"
            }
        }
        return "unknown"
    }

    private func scheduleMaintenanceTimers() {
        let pruneTimer = DispatchSource.makeTimerSource(queue: queue)
        pruneTimer.schedule(deadline: .now() + .seconds(5), repeating: .seconds(5))
        pruneTimer.setEventHandler { [weak self] in
            self?.pruneExpiredCapsules()
            self?.writeSnapshot()
        }
        pruneTimer.resume()
        self.pruneTimer = pruneTimer

        let advertisementPollTimer = DispatchSource.makeTimerSource(queue: queue)
        advertisementPollTimer.schedule(deadline: .now() + .seconds(2), repeating: .seconds(2))
        advertisementPollTimer.setEventHandler { [weak self] in
            self?.refreshAdvertisingFromDisk()
        }
        advertisementPollTimer.resume()
        self.advertisementPollTimer = advertisementPollTimer

        let parentTimer = DispatchSource.makeTimerSource(queue: queue)
        parentTimer.schedule(deadline: .now() + .seconds(5), repeating: .seconds(5))
        parentTimer.setEventHandler { [weak self] in
            guard let self else { return }
            if getppid() != self.parentPID && getppid() == 1 {
                exit(0)
            }
        }
        parentTimer.resume()
        self.parentTimer = parentTimer
    }

    private func nowMillis() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000.0)
    }
}

func value(after flag: String, in arguments: [String]) -> String? {
    guard let index = arguments.firstIndex(of: flag), arguments.indices.contains(index + 1) else {
        return nil
    }
    return arguments[index + 1]
}

let arguments = CommandLine.arguments
guard let snapshotPath = value(after: "--snapshot-file", in: arguments) else {
    fputs(
        "usage: BleAssistBridge.swift --snapshot-file /path/to/ble-assist-bridge.json --advertisement-file /path/to/ble-assist-advertisement.json\n",
        stderr
    )
    exit(64)
}
guard let advertisementPath = value(after: "--advertisement-file", in: arguments) else {
    fputs("missing required --advertisement-file path\n", stderr)
    exit(64)
}

let snapshotURL = URL(fileURLWithPath: snapshotPath)
let advertisementURL = URL(fileURLWithPath: advertisementPath)

let runtime = BleBridgeRuntime(
    snapshotURL: snapshotURL,
    advertisementURL: advertisementURL
)
withExtendedLifetime(runtime) {
    dispatchMain()
}
