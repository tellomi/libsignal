//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

import Foundation
import LibSignalClient
import XCTest

/// The shape of `rust/links/tests/data/bridge-golden.json`.
private struct Golden: Decodable {
    struct Classify: Decodable {
        let name: String
        let preview: String
        let body: String
        let message: String
        let card: String
        let receiveCheck: String
    }

    struct OpenPlan: Decodable {
        let url: String
        let plan: String
    }

    struct Identify: Decodable {
        let url: String
        let location: Bool
        let result: String?
    }

    struct Layout: Decodable {
        let width: UInt32
        let height: UInt32
        let kind: String
        let level: String
        let layout: String
    }

    struct Tint: Decodable {
        let layout: String
        let width: UInt32
        let height: UInt32
        let rgbaHex: String
        let tint: String
    }

    struct Reply: Decodable {
        let status: UInt32
        let finalUrl: String
        let contentType: String
        let location: String?
        let body: String
    }

    struct Script: Decodable {
        let responses: [String: Reply]?
        let firstParty: String?
        let imageOk: Bool?
        let networkError: [String]?
    }

    struct Send: Decodable {
        let name: String
        let url: String
        let context: String
        let script: Script
        let requests: [String]
        let outcome: String
    }

    struct Request: Decodable {
        let id: UInt32
        let type: String
        let url: String?
    }

    let registry: String
    let registryVersion: UInt64
    let degraded: String
    let classify: [Classify]
    let openPlan: [OpenPlan]
    let identify: [Identify]
    let layout: [Layout]
    let tint: [Tint]
    let send: [Send]
}

/// Replays `rust/links/tests/data/bridge-golden.json` through the FFI bridge and compares every
/// result byte for byte with what Rust produced (the same file is replayed by the Kotlin and
/// TypeScript tests).
class LinksTests: TestCaseBase {
    private static let data = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .appendingPathComponent("../../../rust/links/tests/data")
        .standardizedFileURL

    private static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    private func golden() throws -> Golden {
        let bytes = try Data(contentsOf: Self.data.appendingPathComponent("bridge-golden.json"))
        return try Self.decoder.decode(Golden.self, from: bytes)
    }

    private func registry(_ golden: Golden) throws -> LinkRegistry {
        try LinkRegistry.load(Data(contentsOf: Self.data.appendingPathComponent(golden.registry)))
    }

    func testLoadsTheShippedRegistry() throws {
        let golden = try golden()
        let registry = try registry(golden)
        XCTAssertEqual(registry.version, golden.registryVersion)
        XCTAssertEqual(try registry.degraded(), golden.degraded)
    }

    func testClassifyAndReceiveCheck() throws {
        let golden = try golden()
        let registry = try registry(golden)
        for c in golden.classify {
            XCTAssertEqual(try registry.classify(preview: c.preview, body: c.body, message: c.message), c.card, c.name)
            XCTAssertEqual(
                try registry.receiveCheck(preview: c.preview, body: c.body, message: c.message),
                c.receiveCheck,
                c.name
            )
        }
    }

    func testOpenPlanAndIdentify() throws {
        let golden = try golden()
        let registry = try registry(golden)
        for c in golden.openPlan {
            XCTAssertEqual(try registry.openPlan(c.url), c.plan, c.url)
        }
        for c in golden.identify {
            XCTAssertEqual(try registry.identify(c.url, location: c.location), c.result, c.url)
        }
    }

    func testLayoutAndTint() throws {
        let golden = try golden()
        for c in golden.layout {
            let layout = try Links.layout(imageWidth: c.width, imageHeight: c.height, kind: c.kind, level: c.level)
            XCTAssertEqual(layout, c.layout)
        }
        for c in golden.tint {
            var rgba = Data()
            var index = c.rgbaHex.startIndex
            while index < c.rgbaHex.endIndex {
                let next = c.rgbaHex.index(index, offsetBy: 2)
                rgba.append(try XCTUnwrap(UInt8(c.rgbaHex[index..<next], radix: 16)))
                index = next
            }
            let tint = try Links.tint(layout: c.layout, width: c.width, height: c.height, rgba: rgba)
            XCTAssertEqual(tint, c.tint)
        }
    }

    func testSendEveryRequestAndTheOutcome() throws {
        let golden = try golden()
        let registry = try registry(golden)
        for c in golden.send {
            let job = try registry.begin(c.url, context: c.context)
            var requests: [String] = []
            while let request = try job.nextRequest() {
                requests.append(request)
                let parsed = try Self.decoder.decode(Golden.Request.self, from: Data(request.utf8))
                let url = parsed.url ?? ""
                switch parsed.type {
                case "first_party":
                    try job.onFirstParty(id: parsed.id, result: c.script.firstParty ?? "{}")
                case "image":
                    try job.onImage(id: parsed.id, ok: c.script.imageOk ?? false)
                default:
                    if let reply = c.script.responses?[url] {
                        try job.onResponse(
                            id: parsed.id,
                            status: reply.status,
                            finalUrl: reply.finalUrl,
                            contentType: reply.contentType,
                            location: reply.location,
                            body: Data(reply.body.utf8)
                        )
                    } else if c.script.networkError?.contains(url) ?? false {
                        try job.onNetworkError(id: parsed.id)
                    } else {
                        try job.onFailure(id: parsed.id)
                    }
                }
            }
            XCTAssertEqual(requests, c.requests, c.name)
            XCTAssertEqual(try job.finish(), c.outcome, c.name)
        }
    }

    func testBadJsonIsAnErrorNotACrash() throws {
        let registry = try registry(try golden())
        XCTAssertThrowsError(try registry.classify(preview: "{", body: "", message: "{}"))
        XCTAssertThrowsError(try Links.layout(imageWidth: 1, imageHeight: 1, kind: "", level: "nope"))
    }
}
