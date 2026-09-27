//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

import Foundation
import SignalFfi

/// The link registry (ADR-0063): which provider and kind a URL is, what card a stored preview
/// gets, what a tap does. A thin wrapper over the `signal_link_registry_*` functions; every
/// result is JSON text, byte-identical to what Android and Desktop get for the same input
/// (`rust/links/tests/data/bridge-golden.json`). Decode it with `JSONDecoder` / `JSONSerialization`.
///
/// Load it once and keep it; a hot update builds a new one with ``loadUpdate(_:signatureHex:publicKey:currentVersion:)``.
public class LinkRegistry: NativeHandleOwner<SignalMutPointerLinkRegistry>, @unchecked Sendable {
    override internal class func destroyNativeHandle(
        _ handle: NonNull<SignalMutPointerLinkRegistry>
    ) -> SignalFfiErrorRef? {
        return signal_link_registry_destroy(handle.pointer)
    }

    /// The registry shipped with the app (`links-<version>.json`).
    public static func load<Bytes: ContiguousBytes>(_ envelope: Bytes) throws -> LinkRegistry {
        let handle = try envelope.withUnsafeBorrowedBuffer { buffer in
            try invokeFnReturningValueByPointer(.init()) {
                signal_link_registry_load($0, buffer)
            }
        }
        return LinkRegistry(owned: NonNull(handle)!)
    }

    /// A hot update: signature → name → schema → strictly newer than `currentVersion` → content
    /// rules. Throws when any check fails; keep using the current registry then.
    ///
    /// - parameter signatureHex: the `.sig` file's contents
    /// - parameter publicKey: the update key, 33 bytes (`0x05` prefix) or 32
    /// - parameter currentVersion: the version in use, or `nil`
    public static func loadUpdate<Bytes: ContiguousBytes, Key: ContiguousBytes>(
        _ envelope: Bytes,
        signatureHex: String,
        publicKey: Key,
        currentVersion: UInt64?
    ) throws -> LinkRegistry {
        let handle = try envelope.withUnsafeBorrowedBuffer { buffer in
            try publicKey.withUnsafeBorrowedBuffer { key in
                try invokeFnReturningValueByPointer(.init()) {
                    signal_link_registry_load_update($0, buffer, signatureHex, key, currentVersion ?? 0)
                }
            }
        }
        return LinkRegistry(owned: NonNull(handle)!)
    }

    public var version: UInt64 {
        return failOnError {
            try withNativeHandle { handle in
                try invokeFnReturningInteger { signal_link_registry_version($0, handle.const()) }
            }
        }
    }

    /// What this build could not honour and turned into the default (JSON array).
    public func degraded() throws -> String {
        try withNativeHandle { handle in
            try invokeFnReturningString { signal_link_registry_degraded($0, handle.const()) }
        }
    }

    /// The Matcher's view of one URL (JSON), or `nil` when no provider claims it.
    public func identify(_ url: String, location: Bool = false) throws -> String? {
        try withNativeHandle { handle in
            try invokeFnReturningOptionalString {
                signal_link_registry_identify($0, handle.const(), url, location)
            }
        }
    }

    /// The card for a stored preview (JSON). `preview` / `message` are JSON; `rich` is hex.
    public func classify(preview: String, body: String, message: String = "{}") throws -> String {
        try withNativeHandle { handle in
            try invokeFnReturningString {
                signal_link_registry_classify($0, handle.const(), preview, body, message)
            }
        }
    }

    /// Whether to keep the preview and its `rich` when a message arrives (JSON).
    public func receiveCheck(preview: String, body: String, message: String = "{}") throws -> String {
        try withNativeHandle { handle in
            try invokeFnReturningString {
                signal_link_registry_receive_check($0, handle.const(), preview, body, message)
            }
        }
    }

    /// What tapping this URL does (JSON).
    public func openPlan(_ url: String) throws -> String {
        try withNativeHandle { handle in
            try invokeFnReturningString { signal_link_registry_open_plan($0, handle.const(), url) }
        }
    }

    /// Start previewing `url` as typed; `context` is the send context as JSON.
    public func begin(_ url: String, context: String = "{}") throws -> LinkJob {
        let job = try withNativeHandle { handle in
            try invokeFnReturningValueByPointer(.init()) {
                signal_link_registry_begin($0, handle.const(), url, context)
            }
        }
        return LinkJob(owned: NonNull(job)!)
    }
}

/// One link being previewed by the sender. The crate never touches the network: ask for the
/// ``nextRequest()``, perform it with the app's own fetcher, report it, repeat, then ``finish()``.
public class LinkJob: NativeHandleOwner<SignalMutPointerLinkJob> {
    override internal class func destroyNativeHandle(
        _ handle: NonNull<SignalMutPointerLinkJob>
    ) -> SignalFfiErrorRef? {
        return signal_link_job_destroy(handle.pointer)
    }

    /// The next request (JSON), or `nil`: then call ``finish()``.
    public func nextRequest() throws -> String? {
        try withNativeHandle { handle in
            try invokeFnReturningOptionalString { signal_link_job_next_request($0, handle) }
        }
    }

    public func onResponse<Body: ContiguousBytes>(
        id: UInt32,
        status: UInt32,
        finalUrl: String,
        contentType: String,
        location: String?,
        body: Body
    ) throws {
        try withNativeHandle { handle in
            try body.withUnsafeBorrowedBuffer { buffer in
                try withOptionalCString(location) { location in
                    try checkError(
                        signal_link_job_on_response(handle, id, status, finalUrl, contentType, location, buffer)
                    )
                }
            }
        }
    }

    /// DNS / TCP / TLS failure: the host is remembered as unreachable.
    public func onNetworkError(id: UInt32) throws {
        try withNativeHandle { handle in try checkError(signal_link_job_on_network_error(handle, id)) }
    }

    /// Any other failure (timeout after connecting, too large, a rejected hop…).
    public func onFailure(id: UInt32) throws {
        try withNativeHandle { handle in try checkError(signal_link_job_on_failure(handle, id)) }
    }

    /// The result of a `first_party` request, as JSON.
    public func onFirstParty(id: UInt32, result: String) throws {
        try withNativeHandle { handle in try checkError(signal_link_job_on_first_party(handle, id, result)) }
    }

    public func onImage(id: UInt32, ok: Bool) throws {
        try withNativeHandle { handle in try checkError(signal_link_job_on_image(handle, id, ok)) }
    }

    /// The preview to send (JSON); `preview.rich_hex` goes into `Preview` field 1000. (The policy
    /// step runs when the app hands a policy engine to the bridge; this wrapper does not yet.)
    public func finish() throws -> String {
        try withNativeHandle { handle in
            try invokeFnReturningString {
                signal_link_job_finish($0, handle, SignalConstPointerPolicyEngine(raw: nil))
            }
        }
    }
}

/// The card's shape and colours, the same on every platform.
public enum Links {
    /// The card shape for an image of this size (0 × 0 = none), a kind and a level name.
    public static func layout(imageWidth: UInt32, imageHeight: UInt32, kind: String, level: String) throws -> String {
        try invokeFnReturningString { signal_links_layout($0, imageWidth, imageHeight, kind, level) }
    }

    /// Card colours from the card's own image, decoded to RGBA (JSON).
    public static func tint<Pixels: ContiguousBytes>(
        layout: String,
        width: UInt32,
        height: UInt32,
        rgba: Pixels
    ) throws -> String {
        try rgba.withUnsafeBorrowedBuffer { buffer in
            try invokeFnReturningString { signal_links_tint($0, layout, width, height, buffer) }
        }
    }
}

private func withOptionalCString<R>(_ value: String?, _ body: (UnsafePointer<CChar>?) throws -> R) rethrows -> R {
    guard let value else {
        return try body(nil)
    }
    return try value.withCString(body)
}

extension SignalMutPointerLinkRegistry: SignalMutPointer {
    public typealias ConstPointer = SignalConstPointerLinkRegistry

    public init(untyped: OpaquePointer?) {
        self.init(raw: untyped)
    }

    public func toOpaque() -> OpaquePointer? {
        self.raw
    }

    public func const() -> Self.ConstPointer {
        Self.ConstPointer(raw: self.raw)
    }
}

extension SignalConstPointerLinkRegistry: SignalConstPointer {
    public func toOpaque() -> OpaquePointer? {
        self.raw
    }
}

extension SignalMutPointerLinkJob: SignalMutPointer {
    public typealias ConstPointer = OpaquePointer?

    public init(untyped: OpaquePointer?) {
        self.init(raw: untyped)
    }

    public func toOpaque() -> OpaquePointer? {
        self.raw
    }

    public func const() -> Self.ConstPointer {
        nil
    }
}
