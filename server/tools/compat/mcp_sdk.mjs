// SPDX-License-Identifier: Apache-2.0
// Local A3 rehearsal through pinned official clients. Run once per SDK.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const [era, sdkRoot] = process.argv.slice(2);
assert.ok(['modern', 'legacy'].includes(era) && sdkRoot, 'usage: node mcp_sdk.mjs modern|legacy SDK_ROOT');
const modern = era === 'modern';
const revision = modern ? '2026-07-28' : '2025-11-25';
const sdk = modern ? '@modelcontextprotocol/client' : '@modelcontextprotocol/sdk';
const version = modern ? '2.4.0' : '1.32.2';
const require = createRequire(resolve(sdkRoot, 'package.json'));
const installed = JSON.parse(readFileSync(resolve(sdkRoot, 'node_modules', sdk, 'package.json'), 'utf8'));
assert.equal(installed.version, version);
const { Client } = modern ? require(sdk) : require(sdk + '/client/index.js');
const { StreamableHTTPClientTransport } = modern ? require(sdk) : require(sdk + '/client/streamableHttp.js');
const port = Number(process.env.MUNARIUM_COMPAT_MCP_PORT ?? 29080);
assert.ok(Number.isInteger(port) && port > 0 && port <= 65535);
const url = new URL('http://127.0.0.1:' + port);
const mgmt = process.env.MUNARIUM_COMPAT_MCP_MGMT;
assert.ok(mgmt, 'set MUNARIUM_COMPAT_MCP_MGMT to the disposable management token');
const uid = 'compat-sdk-' + era;
const wire = [];
const clients = [];
let dropOpenReply = false;
let dropped = false;

async function post(path, token, body) {
  const response = await fetch(new URL(path, url), {
    method: 'POST',
    headers: {
      authorization: 'Bearer ' + token,
      'content-type': 'application/json',
      'x-munarium-uid': uid,
    },
    body: JSON.stringify(body),
    redirect: 'error',
  });
  assert.ok(response.ok, path + ' returned HTTP ' + response.status);
  return response.json();
}

async function issue(scopes, ttl) {
  const { token } = await post('/v1/access-tokens', mgmt, {
    uid, access_level: 0, scopes, compartments: [], ttl_secs: ttl,
  });
  assert.ok(token);
  return token;
}

function makeClient(token) {
  const client = new Client(
    { name: 'munarium-compat', version },
    modern ? { versionNegotiation: { mode: { pin: revision } } } : {},
  );
  client.onerror = () => {}; // The assertion on the awaited call records the deliberate disconnect.
  clients.push(client);
  const transport = new StreamableHTTPClientTransport(new URL('/mcp', url), {
    requestInit: { headers: { authorization: 'Bearer ' + token }, redirect: 'error' },
    fetch: async (input, init) => {
      const rpc = typeof init?.body === 'string' ? JSON.parse(init.body) : {};
      const response = await fetch(input, init);
      const event = {
        method: rpc.method ?? init?.method ?? 'GET',
        tool: rpc.method === 'tools/call' ? rpc.params.name : undefined,
        version: new Headers(init?.headers).get('mcp-protocol-version'),
        status: response.status,
        challenge: response.status === 401 ? response.headers.get('www-authenticate') : undefined,
        sessionHeader: response.headers.has('mcp-session-id'),
      };
      wire.push(event);
      if (dropOpenReply && event.tool === 'open_session' && response.ok) {
        dropOpenReply = false;
        dropped = true;
        await response.body.cancel();
        throw new Error('simulated lost response');
      }
      return response;
    },
  });
  // The SDK's discovery or initialization generates the real wire requests.
  return { client, transport };
}

async function connect(token) {
  const { client, transport } = makeClient(token);
  await client.connect(transport);
  assert.ok(client.getServerCapabilities().tools);
  if (modern) {
    assert.ok(client.getDiscoverResult().supportedVersions.includes(revision));
    assert.ok(client.getDiscoverResult().supportedVersions.includes('2025-11-25'));
  } else {
    assert.equal(transport.protocolVersion, revision);
  }
  return client;
}

function key() {
  return { id: randomUUID(), expires_at: new Date(Date.now() + 600_000).toISOString() };
}

function success(result) {
  assert.equal(Boolean(result.isError), false, JSON.stringify(result.structuredContent));
  return result.structuredContent;
}

function problem(result, slug) {
  assert.equal(result.isError, true);
  assert.equal(result.structuredContent.type.split('/').at(-1), slug);
  return result.structuredContent;
}

const call = (client, name, args = {}) => client.callTool({ name, arguments: args });
try {
  const token = await issue(['mcp', 'query'], 180);
  const client = await connect(token);
  const listed = (await client.listTools()).tools.map((tool) => tool.name);
  assert.equal(listed.length, 9);
  assert.equal(new Set(listed).size, 9);
  const collections = success(await call(client, 'list_collections'));
  assert.match(JSON.stringify(collections), /starter-documents/);
  const search = { collection: 'starter-documents', query: 'invoice questions', top_k: 1, operation_key: key() };
  const hit = success(await call(client, 'search_collection', search));
  assert.match(JSON.stringify(hit), /billing team/);

  const scoped = await connect(await issue(['mcp'], 180));
  problem(await call(scoped, 'search_collection', { ...search, operation_key: key() }), 'scope-missing');

  const opening = { runbook: 'starter@1', operation_key: key() };
  dropOpenReply = true;
  await assert.rejects(call(client, 'open_session', opening));
  assert.ok(dropped);
  const recovered = await connect(token);
  const replay = success(await call(recovered, 'open_session', opening));
  const receipt = success(await call(recovered, 'open_session', { operation_key: opening.operation_key }));
  assert.equal(receipt.status, 'completed');
  const session = receipt.outcome.session_id;
  assert.match(session, /^ses-/);
  assert.equal(replay.outcome.session_id, session);
  assert.equal(replay.invocation_id, receipt.invocation_id);

  const getSession = (await recovered.listTools()).tools.find((tool) => tool.name === 'get_session');
  const maxTurns = getSession.inputSchema.properties.limit.maximum;
  for (let i = 0; i < maxTurns; i++) {
    await post('/v1/sessions/' + session + '/turns', token, {
      query: 'invoice questions ' + 'invoice '.repeat(200) + i, complete: false, top_k: 1,
    });
  }
  const oversize = problem(await call(recovered, 'get_session', { session_id: session, limit: maxTurns }), 'result-too-large');
  assert.match(oversize.detail, /smaller limit/);
  const page = success(await call(recovered, 'get_session', { session_id: session, limit: 1 }));
  assert.equal(page.turns.length, 1);

  const short = await issue(['mcp', 'query'], 3);
  const expiring = await connect(short);
  const expiry = JSON.parse(Buffer.from(short.split('.')[1], 'base64url').toString()).exp;
  assert.ok(Number.isInteger(expiry));
  // Wait past the Server's 30-second JWT clock-skew allowance.
  await delay(Math.max(0, expiry * 1000 + 32_000 - Date.now()));
  await assert.rejects(expiring.listTools());
  assert.match(wire.findLast((event) => event.status === 401).challenge, /^Bearer/i);
  const renewed = await connect(await issue(['mcp', 'query'], 180));
  assert.equal((await renewed.listTools()).tools.length, 9);
  const recoveredAfterRenewal = success(await call(renewed, 'open_session', { operation_key: opening.operation_key }));
  assert.equal(recoveredAfterRenewal.invocation_id, receipt.invocation_id);
  assert.equal(recoveredAfterRenewal.outcome.session_id, session);
  const admin = await fetch(new URL('/admin/mcp', url), {
    headers: { authorization: 'Bearer ' + mgmt }, redirect: 'error',
  });
  assert.equal(admin.status, 200);
  const html = await admin.text();
  assert.match(html, /MCP/);
  assert.ok(wire.some((event) => event.method === (modern ? 'server/discover' : 'notifications/initialized')));
  assert.ok(wire.some((event) => event.method === 'tools/call' && event.version === revision));
  assert.ok(wire.every((event) => !event.sessionHeader));
  console.log(JSON.stringify({
    sdk, version, revision, evidence_class: 'local', transport: 'streamable-http',
    discovery: modern, tools: listed, syntheticHit: true, denied: 'scope-missing',
    lostReplyRecovery: true, expiredBearerChallenge: true, renewed: true,
    oversize: oversize.detail, smallerPage: page.turns.length,
    adminStatus: admin.status, adminMcpView: true, wire,
  }, null, 2));
} catch (error) {
  console.error(JSON.stringify({ sdk, version, revision, error: error.message, wire }, null, 2));
  process.exitCode = 1;
} finally {
  await Promise.allSettled(clients.map((client) => client.close()));
}
