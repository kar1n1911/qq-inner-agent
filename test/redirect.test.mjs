import test from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import https from 'node:https';
import tls from 'node:tls';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { createHttpsRedirectProxy, createDashboard, closeDashboardListeners } from '../src/dashboard.mjs';

function first(emitter, event) {
  return once(emitter, event).then(args => args[0]);
}
async function listen(server) {
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server.address().port;
}
// The record header of a TLS ClientHello; the proxy must forward it, never redirect it.
const TLS_HELLO = Buffer.from([0x16, 0x03, 0x01, 0x00, 0x05, 0x01, 0x00, 0x00, 0x01, 0x00]);
const KNOWN = new Set(['100.114.145.17:5098']);

async function proxyTo(handler, options = {}) {
  const upstream = net.createServer(handler);
  const upstreamPort = await listen(upstream);
  const proxy = createHttpsRedirectProxy({ upstreamPort, ...options });
  const port = await listen(proxy.server);
  return { upstream, proxy, port };
}
async function redirectFor(port, request) {
  const client = net.connect(port, '127.0.0.1');
  let text = '';
  client.on('data', chunk => { text += chunk; });
  await once(client, 'connect');
  client.write(request);
  const deadline = Date.now() + 2000;
  while (!text && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 10));
  client.destroy();
  return text.split('\r\n\r\n')[0];
}

test('plaintext HTTP on the dashboard port redirects to HTTPS on the same host and path', async () => {
  const { upstream, proxy, port } = await proxyTo(socket => socket.end(), { knownHosts: KNOWN });
  try {
    const head = await redirectFor(port, 'GET /index.html?x=1 HTTP/1.1\r\nHost: 100.114.145.17:5098\r\n\r\n');
    assert.match(head, /^HTTP\/1\.1 301 Moved Permanently/);
    assert.match(head, /Location: https:\/\/100\.114\.145\.17:\d+\/index\.html\?x=1/);
    assert.match(head, /Content-Length: 75/);
    assert.match(head, /Connection: close/);
  } finally { await proxy.close(); upstream.close(); }
});

test('a Host that is not an address the dashboard serves cannot choose the redirect target', async () => {
  const { upstream, proxy, port } = await proxyTo(socket => socket.end(), { knownHosts: KNOWN });
  try {
    const head = await redirectFor(port, 'GET / HTTP/1.1\r\nHost: evil.example\r\n\r\n');
    // The untrusted host is dropped in favour of the address the client actually reached.
    assert.doesNotMatch(head, /evil\.example/);
    assert.match(head, /Location: https:\/\/127\.0\.0\.1:\d+\//);
  } finally { await proxy.close(); upstream.close(); }
});

test('requests are redirected even when no address is configured at all', async () => {
  const { upstream, proxy, port } = await proxyTo(socket => socket.end());
  try {
    const head = await redirectFor(port, 'GET /app.js HTTP/1.1\r\nHost: 100.114.145.17:5098\r\n\r\n');
    assert.match(head, /Location: https:\/\/127\.0\.0\.1:\d+\/app\.js/);
  } finally { await proxy.close(); upstream.close(); }
});

test('a TLS handshake is forwarded to the upstream server untouched', async () => {
  const { upstream, proxy, port } = await proxyTo(socket => socket.end('upstream-saw-me'), { knownHosts: KNOWN });
  try {
    const client = net.connect(port, '127.0.0.1');
    await once(client, 'connect');
    client.write(TLS_HELLO);
    assert.equal((await first(client, 'data')).toString(), 'upstream-saw-me');
    await once(client, 'close');
  } finally { await proxy.close(); upstream.close(); }
});

test('an unreachable upstream closes the client instead of hanging', async () => {
  const proxy = createHttpsRedirectProxy({ upstreamPort: 1 });
  const port = await listen(proxy.server);
  try {
    const client = net.connect(port, '127.0.0.1');
    await once(client, 'connect');
    client.write(TLS_HELLO);
    await once(client, 'close');
  } finally { await proxy.close(); }
});


test('redirect connections release resources without opening upstream sockets or accumulating listeners', { timeout: 5000 }, async () => {
  let connections = 0;
  const { upstream, proxy, port } = await proxyTo(socket => { connections++; socket.destroy(); });
  const listeners = proxy.server.listenerCount('close');
  try {
    for (let i = 0; i < 20; i++) {
      assert.match(await redirectFor(port, 'GET / HTTP/1.1\r\nHost: localhost\r\n\r\n'), /301/);
    }
    assert.equal(connections, 0);
    assert.equal(proxy.server.listenerCount('close'), listeners);
  } finally { await proxy.close(); upstream.close(); }
});

test('real TLS requests retain distinct login limits and shutdown closes idle connections', {
  timeout: 10000,
  skip: process.platform === 'darwin' && 'macOS does not configure the second loopback address 127.0.0.2 by default',
}, async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-proxy-test-'));
  let proxy, upstream, dashboard, idle, encrypted;
  try {
    const keyFile = path.join(root, 'key.pem'), certFile = path.join(root, 'cert.pem');
    execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', keyFile,
      '-out', certFile, '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=IP:127.0.0.1'], { stdio: 'ignore' });
    const cert = fs.readFileSync(certFile);
    upstream = https.createServer({ key: fs.readFileSync(keyFile), cert });
    const upstreamPort = await listen(upstream);
    proxy = createHttpsRedirectProxy({ upstreamPort });
    const port = await listen(proxy.server), origin = `https://127.0.0.1:${port}`;
    dashboard = createDashboard({ root, settings: { origins: [origin] }, key: 'test-only-access' });
    upstream.on('request', (req, res) => dashboard.handler(req, res, { address: proxy.peerFor(req, req.socket.remoteAddress) }));
    const login = (localAddress, key) => new Promise((resolve, reject) => {
      const req = https.request(origin + '/api/login', { method: 'POST', agent: false, ca: cert, localAddress,
        headers: { Origin: origin, 'Content-Type': 'application/json', 'X-Forwarded-For': '198.51.100.1' } }, res => {
        res.resume(); res.on('end', () => resolve(res.statusCode));
      });
      req.on('error', reject); req.end(JSON.stringify({ key }));
    });
    for (let i = 0; i < 8; i++) assert.equal(await login('127.0.0.1', 'wrong'), 401);
    assert.equal(await login('127.0.0.1', 'test-only-access'), 429);
    assert.equal(await login('127.0.0.2', 'test-only-access'), 200);
    // Exercise both a socket waiting for its first byte and an established TLS hop.
    idle = net.connect(port, '127.0.0.1'); await once(idle, 'connect');
    encrypted = tls.connect({ port, host: '127.0.0.1', ca: cert }); await once(encrypted, 'secureConnect');
    const closed = Promise.all([once(idle, 'close'), once(encrypted, 'close')]);
    await closeDashboardListeners({ redirectProxy: proxy, upstream });
    await closed;
    assert.equal(proxy.server.listening, false);
    assert.equal(upstream.listening, false);
    await closeDashboardListeners({ redirectProxy: proxy, upstream }); // idempotent shutdown
  } finally {
    idle?.destroy(); encrypted?.destroy();
    await closeDashboardListeners({ redirectProxy: proxy, upstream });
    await dashboard?.close();
    fs.rmSync(root, { recursive: true, force: true });
  }
});
