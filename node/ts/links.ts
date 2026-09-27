//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

/**
 * Link cards (ADR-0063). A thin wrapper over the `LinkRegistry_*` / `LinkJob_*` / `Links_*`
 * bridge functions: every result is JSON text, byte-identical to what Android and iOS get for the
 * same input (`rust/links/tests/data/bridge-golden.json`); parse it with `JSON.parse`.
 *
 * The crate never touches the network. The sender drives a {@link LinkJob}: ask for the next
 * request, perform it with the app's own fetcher, report the outcome, repeat, then `finish()`.
 */

import * as Native from './Native.js';

/** Anything holding a policy engine handle (`PolicyEngine_Load`). */
export type PolicyEngineHandle = {
  readonly _nativeHandle: Native.PolicyEngine;
};

export class LinkRegistry {
  readonly _nativeHandle: Native.LinkRegistry;

  private constructor(nativeHandle: Native.LinkRegistry) {
    this._nativeHandle = nativeHandle;
  }

  /** The registry shipped with the app (`links-<version>.json`). */
  static load(envelope: Uint8Array<ArrayBuffer>): LinkRegistry {
    return new LinkRegistry(Native.LinkRegistry_Load(envelope));
  }

  /**
   * A hot update: signature → name → schema → strictly newer than `currentVersion` → content
   * rules. Throws when any check fails; keep using the current registry then.
   *
   * @param signatureHex the `.sig` file's contents
   * @param publicKey the update key, 33 bytes (`0x05` prefix) or 32
   * @param currentVersion the version in use, or null
   */
  static loadUpdate(
    envelope: Uint8Array<ArrayBuffer>,
    signatureHex: string,
    publicKey: Uint8Array<ArrayBuffer>,
    currentVersion: bigint | null
  ): LinkRegistry {
    return new LinkRegistry(
      Native.LinkRegistry_LoadUpdate(
        envelope,
        signatureHex,
        publicKey,
        currentVersion ?? 0n
      )
    );
  }

  get version(): bigint {
    return Native.LinkRegistry_Version(this);
  }

  /** What this build could not honour and turned into the default (JSON array). */
  degraded(): string {
    return Native.LinkRegistry_Degraded(this);
  }

  /** The Matcher's view of one URL (JSON), or null when no provider claims it. */
  identify(url: string, location = false): string | null {
    return Native.LinkRegistry_Identify(this, url, location);
  }

  /** The card for a stored preview (JSON). `preview` / `message` are JSON; `rich` is hex. */
  classify(preview: string, body: string, message = '{}'): string {
    return Native.LinkRegistry_Classify(this, preview, body, message);
  }

  /** Whether to keep the preview and its `rich` when a message arrives (JSON). */
  receiveCheck(preview: string, body: string, message = '{}'): string {
    return Native.LinkRegistry_ReceiveCheck(this, preview, body, message);
  }

  /** What tapping this URL does (JSON). */
  openPlan(url: string): string {
    return Native.LinkRegistry_OpenPlan(this, url);
  }

  /** Start previewing `url` as typed; `context` is the send context as JSON. */
  begin(url: string, context = '{}'): LinkJob {
    return new LinkJob(Native.LinkRegistry_Begin(this, url, context));
  }
}

export class LinkJob {
  readonly _nativeHandle: Native.LinkJob;

  /** @internal */
  constructor(nativeHandle: Native.LinkJob) {
    this._nativeHandle = nativeHandle;
  }

  /** The next request (JSON), or null: then call `finish()`. */
  nextRequest(): string | null {
    return Native.LinkJob_NextRequest(this);
  }

  onResponse(
    id: number,
    status: number,
    finalUrl: string,
    contentType: string,
    location: string | null,
    body: Uint8Array<ArrayBuffer>
  ): void {
    Native.LinkJob_OnResponse(
      this,
      id,
      status,
      finalUrl,
      contentType,
      location,
      body
    );
  }

  /** DNS / TCP / TLS failure: the host is remembered as unreachable. */
  onNetworkError(id: number): void {
    Native.LinkJob_OnNetworkError(this, id);
  }

  /** Any other failure (timeout after connecting, too large, a rejected hop…). */
  onFailure(id: number): void {
    Native.LinkJob_OnFailure(this, id);
  }

  /** The result of a `first_party` request, as JSON. */
  onFirstParty(id: number, result: string): void {
    Native.LinkJob_OnFirstParty(this, id, result);
  }

  onImage(id: number, ok: boolean): void {
    Native.LinkJob_OnImage(this, id, ok);
  }

  /** The preview to send (JSON); `preview.rich_hex` goes into `Preview` field 1000. */
  finish(policy: PolicyEngineHandle | null = null): string {
    return Native.LinkJob_Finish(this, policy);
  }
}

/** The card shape for an image of this size (0 × 0 = none), a kind and a level name. */
export function layout(
  imageWidth: number,
  imageHeight: number,
  kind: string,
  level: string
): string {
  return Native.Links_Layout(imageWidth, imageHeight, kind, level);
}

/** Card colours from the card's own image, decoded to RGBA (JSON). */
export function tint(
  layoutName: string,
  width: number,
  height: number,
  rgba: Uint8Array<ArrayBuffer>
): string {
  return Native.Links_Tint(layoutName, width, height, rgba);
}
