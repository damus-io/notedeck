#!/usr/bin/env node
//
// A logging proxy in front of a local nostr relay, to see what clients actually
// send it: point a client at ws://127.0.0.1:<listen-port> instead of the relay.
//
//   node scripts/relay-wire-log.mjs <listen-port> <relay-port>
//
// Bytes pass through untouched (TLS-less, local relays only). Each websocket
// frame is decoded both ways into one line per nostr message on stdout:
//
//   #<conn> C EVENT kind=1081         what a client sent: EVENT kinds, AUTH as
//   #<conn> C AUTH <pubkey>           whom, REQ / NEG-OPEN / CLOSE and filters
//   #<conn> C REQ ["sub",{...}]
//   #<conn> R EVENT kind=1059         what the relay sent: events, and its
//   #<conn> R ["CLOSED","sub","..."]  refusals (CLOSED, NEG-ERR, NOTICE,
//   #<conn> R OK-false blocked: ...   OK false)
//
// A relay that refuses a plaintext event has still been sent it; this is how to
// tell. Used by scripts/sns-acceptance.
import net from "node:net";

const [listen, upstream] = process.argv.slice(2).map(Number);
if (!listen || !upstream) {
  console.error("usage: relay-wire-log.mjs <listen-port> <relay-port>");
  process.exit(1);
}
let conns = 0;

// A websocket frame decoder for one direction of one connection: skips the
// HTTP upgrade, unmasks client frames, joins fragments, logs each text message.
function decoder(dir, id) {
  let buf = Buffer.alloc(0), open = false, parts = [];
  return (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    if (!open) {
      const end = buf.indexOf("\r\n\r\n");
      if (end < 0) return;
      buf = buf.subarray(end + 4);
      open = true;
    }
    for (;;) {
      if (buf.length < 2) return;
      const fin = buf[0] & 0x80, op = buf[0] & 0x0f, masked = buf[1] & 0x80;
      let len = buf[1] & 0x7f, off = 2;
      if (len === 126) { if (buf.length < 4) return; len = buf.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (buf.length < 10) return; len = Number(buf.readBigUInt64BE(2)); off = 10; }
      const mask = masked ? buf.subarray(off, off + 4) : null;
      if (masked) off += 4;
      if (buf.length < off + len) return;
      const payload = Buffer.from(buf.subarray(off, off + len));
      if (mask) for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
      buf = buf.subarray(off + len);
      if (op > 1) continue; // ping, pong, close
      parts.push(payload);
      if (fin) { log(dir, id, Buffer.concat(parts).toString()); parts = []; }
    }
  };
}

function log(dir, id, text) {
  let m;
  try { m = JSON.parse(text); } catch { return; }
  let line;
  if (dir === "C" && m[0] === "EVENT") line = `EVENT kind=${m[1].kind}`;
  else if (dir === "C" && m[0] === "AUTH") line = `AUTH ${m[1].pubkey}`;
  else if (dir === "C") line = `${m[0]} ${JSON.stringify(m.slice(1, 3)).slice(0, 300)}`;
  else if (m[0] === "EVENT") line = `EVENT kind=${m[2].kind}`;
  else if (m[0] === "OK" && !m[2]) line = `OK-false ${m[3]}`;
  else if (["CLOSED", "NEG-ERR", "NOTICE"].includes(m[0])) line = JSON.stringify(m);
  if (line) console.log(`#${id} ${dir} ${line}`);
}

net.createServer((client) => {
  const id = ++conns, up = net.connect(upstream, "127.0.0.1");
  const c = decoder("C", id), r = decoder("R", id);
  client.on("data", (d) => { c(d); up.write(d); });
  up.on("data", (d) => { r(d); client.write(d); });
  for (const [a, b] of [[client, up], [up, client]]) {
    a.on("close", () => b.destroy());
    a.on("error", () => b.destroy());
  }
}).listen(listen, "127.0.0.1");
