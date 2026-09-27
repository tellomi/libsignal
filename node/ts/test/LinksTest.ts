//
// Copyright 2026 重庆半格智能科技有限公司
// SPDX-License-Identifier: AGPL-3.0-only
//

import { assert } from 'chai';
import { Buffer } from 'node:buffer';
import { readFileSync } from 'node:fs';
import * as path from 'node:path';

import * as links from '../links.js';
import * as util from './util.js';

util.initLogger();

const DATA = path.join(import.meta.dirname, '../../../rust/links/tests/data');

type Reply = {
  status: number;
  final_url: string;
  content_type: string;
  location?: string;
  body: string;
};

type Script = {
  responses?: Record<string, Reply>;
  first_party?: string;
  image_ok?: boolean;
  network_error?: string[];
};

/** The shape of `rust/links/tests/data/bridge-golden.json`. */
type Golden = {
  registry: string;
  registry_version: number;
  degraded: string;
  classify: {
    name: string;
    preview: string;
    body: string;
    message: string;
    card: string;
    receive_check: string;
  }[];
  open_plan: { url: string; plan: string }[];
  identify: { url: string; location: boolean; result: string | null }[];
  layout: {
    width: number;
    height: number;
    kind: string;
    level: string;
    layout: string;
  }[];
  tint: {
    layout: string;
    width: number;
    height: number;
    rgba_hex: string;
    tint: string;
  }[];
  send: {
    name: string;
    url: string;
    context: string;
    script: Script;
    requests: string[];
    outcome: string;
  }[];
};

const golden = JSON.parse(
  readFileSync(path.join(DATA, 'bridge-golden.json'), 'utf8')
) as Golden;

function registry(): links.LinkRegistry {
  return links.LinkRegistry.load(
    new Uint8Array(readFileSync(path.join(DATA, golden.registry)))
  );
}

describe('links (ADR-0063): bridge golden, byte for byte', () => {
  it('loads the shipped registry', () => {
    const r = registry();
    assert.equal(r.version, BigInt(golden.registry_version));
    assert.equal(r.degraded(), golden.degraded);
  });

  it('classify / receiveCheck', () => {
    const r = registry();
    for (const c of golden.classify) {
      assert.equal(r.classify(c.preview, c.body, c.message), c.card, c.name);
      assert.equal(
        r.receiveCheck(c.preview, c.body, c.message),
        c.receive_check,
        c.name
      );
    }
  });

  it('openPlan / identify', () => {
    const r = registry();
    for (const c of golden.open_plan) {
      assert.equal(r.openPlan(c.url), c.plan, c.url);
    }
    for (const c of golden.identify) {
      assert.equal(r.identify(c.url, c.location), c.result, c.url);
    }
  });

  it('layout / tint', () => {
    for (const c of golden.layout) {
      assert.equal(links.layout(c.width, c.height, c.kind, c.level), c.layout);
    }
    for (const c of golden.tint) {
      const rgba = new Uint8Array(Buffer.from(c.rgba_hex, 'hex'));
      assert.equal(links.tint(c.layout, c.width, c.height, rgba), c.tint);
    }
  });

  it('send: every request and the outcome', () => {
    const r = registry();
    for (const c of golden.send) {
      const script = c.script;
      const job = r.begin(c.url, c.context);
      const requests: string[] = [];
      for (let req = job.nextRequest(); req !== null; req = job.nextRequest()) {
        requests.push(req);
        const parsed = JSON.parse(req) as {
          id: number;
          type: string;
          url?: string;
        };
        const url = parsed.url ?? '';
        if (parsed.type === 'first_party') {
          job.onFirstParty(parsed.id, script.first_party ?? '{}');
        } else if (parsed.type === 'image') {
          job.onImage(parsed.id, script.image_ok ?? false);
        } else if (script.responses?.[url] !== undefined) {
          const reply = script.responses[url];
          job.onResponse(
            parsed.id,
            reply.status,
            reply.final_url,
            reply.content_type,
            reply.location ?? null,
            new Uint8Array(Buffer.from(reply.body, 'utf8'))
          );
        } else if (script.network_error?.includes(url)) {
          job.onNetworkError(parsed.id);
        } else {
          job.onFailure(parsed.id);
        }
      }
      assert.deepEqual(requests, c.requests, c.name);
      assert.equal(job.finish(), c.outcome, c.name);
    }
  });

  it('bad JSON is an error, not a crash', () => {
    const r = registry();
    assert.throws(() => r.classify('{', '', '{}'));
    assert.throws(() => links.layout(1, 1, '', 'nope'));
  });
});
