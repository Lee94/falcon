import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
  downstreamResponseHeaders,
  px0StatusPage,
  rewriteCsp,
  upstreamRequestHeaders,
} from "./proxy.js";

describe("upstreamRequestHeaders", () => {
  it("drops falcon credentials and hop-by-hop headers, rewrites host", () => {
    const out = upstreamRequestHeaders(
      {
        host: "localhost:4923",
        cookie: "falcon_token=secret",
        authorization: "Bearer x",
        connection: "keep-alive, x-custom-hop",
        "x-custom-hop": "1",
        "keep-alive": "timeout=5",
        "transfer-encoding": "chunked",
        accept: "text/event-stream",
        "content-type": "application/json",
        "content-length": "12",
      },
      "127.0.0.1"
    );
    assert.deepEqual(out, {
      host: "127.0.0.1",
      accept: "text/event-stream",
      "content-type": "application/json",
      "content-length": "12",
    });
  });
});

describe("upstreamRequestHeaders · Origin", () => {
  // px0 的 localPost：Origin 的 host 必须等于 Host，且 Host 是 IP 或 localhost
  it("maps a same-origin POST onto px0's own origin", () => {
    const out = upstreamRequestHeaders(
      { host: "mac-mini.local:6789", origin: "http://mac-mini.local:6789" },
      "127.0.0.1"
    );
    assert.equal(out.host, "127.0.0.1");
    assert.equal(out.origin, "http://127.0.0.1");
  });

  it("forwards a cross-origin Origin untouched so px0 still rejects it", () => {
    for (const origin of ["http://mac-mini.local:7777", "https://evil.example", "null"]) {
      const out = upstreamRequestHeaders({ host: "mac-mini.local:6789", origin }, "127.0.0.1");
      assert.equal(out.origin, origin);
    }
  });

  it("adds no Origin when the browser sent none", () => {
    const out = upstreamRequestHeaders({ host: "localhost:6789" }, "127.0.0.1");
    assert.equal(out.origin, undefined);
  });
});

describe("downstreamResponseHeaders", () => {
  it("rewrites frame-ancestors and drops set-cookie / hop-by-hop", () => {
    const out = downstreamResponseHeaders({
      "content-type": "text/html",
      "content-encoding": "gzip",
      "content-security-policy": "default-src 'self'; frame-ancestors 'none'; form-action 'none';",
      "set-cookie": ["falcon_token=evil; Path=/"],
      connection: "close",
      "transfer-encoding": "chunked",
    });
    assert.deepEqual(out, {
      "content-type": "text/html",
      "content-encoding": "gzip",
      "content-security-policy": "default-src 'self'; frame-ancestors 'self'; form-action 'none';",
    });
  });

  it("leaves the rest of the CSP alone", () => {
    const csp = "default-src 'self'; script-src 'self'; connect-src 'self'";
    assert.equal(rewriteCsp(csp), csp);
  });
});

describe("px0StatusPage", () => {
  it("refreshes back to the entry while starting", () => {
    const html = px0StatusPage({ kind: "starting", stage: "installing" }, "/px0/p1/", "app");
    assert.match(html, /http-equiv="refresh" content="1;url=\/px0\/p1\/"/);
    assert.match(html, /正在把 px0 装到宿主机/);
  });

  it("stops refreshing on error, escapes the message, offers a retry", () => {
    const html = px0StatusPage(
      { kind: "error", message: "<script>alert(1)</script>" },
      "/px0/p1/",
      "a<b"
    );
    assert.doesNotMatch(html, /http-equiv="refresh"/);
    assert.doesNotMatch(html, /<script>/);
    assert.match(html, /&lt;script&gt;/);
    assert.match(html, /px0 · a&lt;b/);
    assert.match(html, /href="\/px0\/p1\/\?retry=1"/);
  });
});
