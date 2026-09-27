//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

package org.signal.libsignal.links

import org.signal.libsignal.internal.Native
import org.signal.libsignal.internal.NativeHandleGuard

/**
 * The link registry (ADR-0063): which provider and kind a URL is, what card a stored preview gets,
 * what a tap does. A thin wrapper over the `LinkRegistry_*` bridge functions; every result is JSON
 * text, byte-identical to what iOS and Desktop get for the same input
 * (`rust/links/tests/data/bridge-golden.json`).
 *
 * Load it once and keep it; a hot update builds a new one with [loadUpdate].
 */
public class LinkRegistry private constructor(
  nativeHandle: Long,
) : NativeHandleGuard.SimpleOwner(NativeHandleGuard.SimpleOwner.throwIfNull(nativeHandle)) {
  public companion object {
    /** The registry shipped with the app (`links-<version>.json`). */
    @JvmStatic
    public fun load(envelope: ByteArray): LinkRegistry = LinkRegistry(Native.LinkRegistry_Load(envelope))

    /**
     * A hot update: signature → name → schema → strictly newer than [currentVersion] → content
     * rules. Throws [IllegalArgumentException] when any check fails; keep the current registry.
     *
     * @param signatureHex the `.sig` file's contents
     * @param publicKey the update key, 33 bytes (`0x05` prefix) or 32
     * @param currentVersion the version in use, or null
     */
    @JvmStatic
    public fun loadUpdate(
      envelope: ByteArray,
      signatureHex: String,
      publicKey: ByteArray,
      currentVersion: Long?,
    ): LinkRegistry =
      LinkRegistry(
        Native.LinkRegistry_LoadUpdate(envelope, signatureHex, publicKey, currentVersion ?: 0L),
      )
  }

  protected override fun release(nativeHandle: Long) {
    Native.LinkRegistry_Destroy(nativeHandle)
  }

  public val version: Long
    get() = guardedMap(Native::LinkRegistry_Version)

  /** What this build could not honour and turned into the default (JSON array). */
  public fun degraded(): String = guardedMap(Native::LinkRegistry_Degraded)

  /** The Matcher's view of one URL (JSON), or null when no provider claims it. */
  @JvmOverloads
  public fun identify(
    url: String,
    location: Boolean = false,
  ): String? = guardedMap { Native.LinkRegistry_Identify(it, url, location) }

  /** The card for a stored preview (JSON). [preview] / [message] are JSON; `rich` is hex. */
  @JvmOverloads
  public fun classify(
    preview: String,
    body: String,
    message: String = "{}",
  ): String = guardedMap { Native.LinkRegistry_Classify(it, preview, body, message) }

  /** Whether to keep the preview and its `rich` when a message arrives (JSON). */
  @JvmOverloads
  public fun receiveCheck(
    preview: String,
    body: String,
    message: String = "{}",
  ): String = guardedMap { Native.LinkRegistry_ReceiveCheck(it, preview, body, message) }

  /** What tapping this URL does (JSON). */
  public fun openPlan(url: String): String = guardedMap { Native.LinkRegistry_OpenPlan(it, url) }

  /** Start previewing [url] as typed; [context] is the send context as JSON. */
  @JvmOverloads
  public fun begin(
    url: String,
    context: String = "{}",
  ): LinkJob = LinkJob(guardedMap { Native.LinkRegistry_Begin(it, url, context) })
}

/**
 * One link being previewed by the sender. The crate never touches the network: ask for the
 * [nextRequest], perform it with the app's own fetcher, report it, repeat, then [finish].
 * Not thread-safe.
 */
public class LinkJob internal constructor(
  nativeHandle: Long,
) : NativeHandleGuard.SimpleOwner(NativeHandleGuard.SimpleOwner.throwIfNull(nativeHandle)) {
  protected override fun release(nativeHandle: Long) {
    Native.LinkJob_Destroy(nativeHandle)
  }

  /** The next request (JSON), or null: then call [finish]. */
  public fun nextRequest(): String? = guardedMap(Native::LinkJob_NextRequest)

  public fun onResponse(
    id: Int,
    status: Int,
    finalUrl: String,
    contentType: String,
    location: String?,
    body: ByteArray,
  ): Unit = guardedRun { Native.LinkJob_OnResponse(it, id, status, finalUrl, contentType, location, body) }

  /** DNS / TCP / TLS failure: the host is remembered as unreachable. */
  public fun onNetworkError(id: Int): Unit = guardedRun { Native.LinkJob_OnNetworkError(it, id) }

  /** Any other failure (timeout after connecting, too large, a rejected hop…). */
  public fun onFailure(id: Int): Unit = guardedRun { Native.LinkJob_OnFailure(it, id) }

  /** The result of a `first_party` request, as JSON. */
  public fun onFirstParty(
    id: Int,
    result: String,
  ): Unit = guardedRun { Native.LinkJob_OnFirstParty(it, id, result) }

  public fun onImage(
    id: Int,
    ok: Boolean,
  ): Unit = guardedRun { Native.LinkJob_OnImage(it, id, ok) }

  /**
   * The preview to send (JSON); `preview.rich_hex` goes into `Preview` field 1000. [policy] is a
   * loaded policy engine (`PolicyEngine_Load`) or null.
   */
  @JvmOverloads
  public fun finish(policy: NativeHandleGuard.Owner? = null): String =
    NativeHandleGuard(policy).use { p -> guardedMap { Native.LinkJob_Finish(it, p.nativeHandle()) } }
}

/** The card's shape and colours, the same on every platform. */
public object Links {
  /** The card shape for an image of this size (0 × 0 = none), a kind and a level name. */
  @JvmStatic
  public fun layout(
    imageWidth: Int,
    imageHeight: Int,
    kind: String,
    level: String,
  ): String = Native.Links_Layout(imageWidth, imageHeight, kind, level)

  /** Card colours from the card's own image, decoded to RGBA (JSON). */
  @JvmStatic
  public fun tint(
    layout: String,
    width: Int,
    height: Int,
    rgba: ByteArray,
  ): String = Native.Links_Tint(layout, width, height, rgba)
}
